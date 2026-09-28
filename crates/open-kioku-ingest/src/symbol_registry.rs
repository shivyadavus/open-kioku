use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, ImportResolution, Language, PackageDeclarationSite, QualityNote,
    QualityNoteKind, ResolutionStatus, Scope, ScopeId, ScopeKind, StringInterner, Symbol, SymbolId,
    SymbolKind, TypeAliasSite,
};
use open_kioku_resolution::{
    context::rust_rules_out_same_file_item, BindingIndex, InheritanceIndex, ResolutionContext,
    ScopeIndex, SymbolIndex,
};
use open_kioku_semantic_model::{ImportBinding, SemanticRepository, GLOB_IMPORT_LOCAL_NAME};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

const COMMON_NAME_CAP: usize = 32;
const MAX_TOKENS_PER_CHUNK: usize = 80;
const MAX_UNRESOLVED_NOTES: usize = 64;
const MAX_SIMPLE_NAMES_FOR_FUZZY: usize = 5000;
/// Aliases one alias may lead through before its target is left unplaced; Go code rarely chains
/// more than two.
const MAX_ALIAS_HOPS: usize = 8;
/// Classes one Java member may be nested in before the registry stops reading its parents.
const MAX_ENCLOSING_CLASSES: usize = 64;

#[derive(Debug, Clone)]
pub struct SymbolRegistry {
    pub by_id: HashMap<SymbolId, Symbol>,
    pub by_qualified_name: HashMap<String, Vec<SymbolId>>,
    pub by_simple_name: HashMap<String, Vec<SymbolId>>,
    pub by_file: HashMap<FileId, Vec<SymbolId>>,
    pub by_module: HashMap<String, Vec<SymbolId>>,
    pub import_resolutions: Vec<ImportResolution>,
    by_file_imports: HashMap<FileId, Vec<usize>>,
    /// Symbols of each file by the names a token can match them under (`symbol_matches_token`):
    /// the simple name and the last segment of the qualified name, in `by_file` order. An
    /// import resolved to a file is read through this for every token of the importing file's
    /// chunks, so it must not scan the target file's symbols per token: a Rust crate root reached
    /// by thousands of cross-crate imports defines thousands of symbols.
    by_file_token: HashMap<(FileId, String), Vec<SymbolId>>,
    by_name_suffix: HashMap<String, Vec<SymbolId>>,
    qualified_name_normalized: HashMap<String, String>,
    /// Every segment of a qualified name before its last (directories, files, modules), with a
    /// crate's `-` spelled `_` as a path spells it.
    places: HashSet<String>,
    /// What each Go type alias stands for, read through its file's imports. A match of the alias
    /// is a match of its target.
    alias_targets: HashMap<SymbolId, AliasTarget>,
    /// Each Go `_test.go` file, whose declarations Go compiles only into the tests of its own
    /// directory's package, by where they can be named from.
    go_test_files: HashMap<FileId, GoTestFile>,
    /// The directory of each Go file, when the repository holds a `_test.go` file.
    go_dirs: HashMap<FileId, String>,
    /// Every package a Java file declares (`org.example`): a Java import from inside one may name
    /// a repository class wherever its file sits.
    java_packages: HashSet<String>,
    /// Every Java file, whose imports alone are read against `java_packages`.
    java_files: HashSet<FileId>,
}

#[derive(Debug, Clone)]
struct GoTestFile {
    dir: String,
    /// Of the external test package (`package store_test` in `store/`), which only its own
    /// files can name, not of the package it tests.
    external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AliasTarget {
    /// The repository type the alias stands for, through every alias on the way.
    Type(SymbolId),
    /// No single repository type could be placed: a predeclared, composite or other module's
    /// type, an import the file does not bind, or a name declared twice in the target package.
    /// Holds the alias's type as written.
    Unplaced(String),
}

/// Where a token's path or import puts its target, for a match by name alone.
#[derive(Clone, Copy)]
enum Origin<'t> {
    /// No spelling says, or a module does, which may re-export the item from anywhere.
    Anywhere,
    /// A path or import whose root is not in the repository: `std::`, `serde_json::`,
    /// `use anyhow::Result;`.
    Outside,
    /// A path through a repository type, whose members are its own: `ScopeKind::File`.
    MemberOf(&'t str),
    /// Java static imports of the name, at least one not from outside: the target is a member
    /// of a class one of them names by its path (`import static org.example.Constants.ACCESS_KEY;`).
    StaticMemberOf(&'t [ImportBinding]),
}

/// Where a member's receiver says the member is defined, for a match by name alone.
#[derive(Clone, Copy)]
enum ReceiverPlace<'t> {
    /// Under a directory, module or type of the receiver's name: `ledger.NewEntry`,
    /// `Constants.ACCESS_KEY`.
    Named,
    /// The receiver is imported from outside the repository.
    Outside,
    /// In the Go package directory the receiver's import path names: `dir` joined with `rest`
    /// (a module's directory and the path below the module), either of which may be empty.
    GoPackage { dir: &'t str, rest: &'t str },
    /// A Go import, with no module declared, whose path ends with no directory of the
    /// repository, which only the root package can be: at the root, under the receiver's name.
    GoRoot,
}

#[derive(Debug, Clone, Default)]
pub struct RegistryReport {
    pub analysis_facts: Vec<AnalysisFact>,
    pub quality_notes: Vec<QualityNote>,
    pub heuristic_hints: Vec<HeuristicRelationshipHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicRelationshipHint {
    pub from: SymbolId,
    pub to: SymbolId,
    pub hint_type: GraphEdgeType,
    pub confidence: Confidence,
}

#[derive(Debug, Clone)]
struct Resolution {
    symbol: Option<Symbol>,
    strategy: &'static str,
    candidates: usize,
    confidence: Confidence,
    ambiguity_reason: Option<String>,
    speculative: bool,
}

#[derive(Debug, Clone)]
struct TokenUse {
    token: String,
    line: u32,
    /// 1-based byte column of the token's first character, as scope ranges count columns.
    column: u32,
    is_call: bool,
    /// Not the tail of a `path::`, the member of a `receiver.` or the name a `mod` item
    /// declares: only a bare name is looked up in the scopes around its use. A `mod` item's
    /// scope covers its own name, so that name would read as a use inside the module it declares.
    bare: bool,
    role: TokenRole,
    /// Rust: the segment before `::` when the token ends a path (`mem` of `std::mem::take`).
    qualifier: Option<String>,
    /// A member's receiver when it is a plain name, outside an import line: `ledger` of
    /// `ledger.NewEntry(..)`, `Constants` of `Constants.ACCESS_KEY`.
    receiver: Option<String>,
}

/// What a token's place in the code says it can name, beyond its spelling (#582). Only a `Name`
/// is matched by name alone across the repository; every role still resolves through an import
/// or the use site's own file, which carry evidence a name match does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenRole {
    Name,
    /// Inside a Rust `#[...]` attribute, or the path of a `@decorator` or `@Annotation`.
    Attribute,
    /// The member of `receiver.member`: which one depends on the receiver's type, which a name
    /// match does not know.
    Member,
    /// A field or parameter name: a Rust `name: value` or `Foo { name, .. }`, a JavaScript or
    /// TypeScript object key or annotated name, a Python keyword argument. Only a field of that
    /// name can be what it names.
    Field,
    /// A name the chunk binds as a local at or before this use.
    Local,
}

impl TokenRole {
    /// Why a name-only strategy did not match, for the caveat of a token no strategy resolved.
    fn withheld_reason(self) -> Option<&'static str> {
        match self {
            TokenRole::Name | TokenRole::Field => None,
            TokenRole::Attribute => Some(
                "attribute or annotation name; a name-only match needs an import or same-file candidate",
            ),
            TokenRole::Member => Some(
                "member access without receiver evidence; a name-only match needs an import or same-file candidate",
            ),
            TokenRole::Local => Some(
                "the chunk binds this name locally; a name-only match needs an import or same-file candidate",
            ),
        }
    }
}

/// The resolver's scope and import model, which lets a Rust bare name match a same-file item
/// only where the resolver's module scoping lets the use site see it (#526).
pub struct RegistryScopeModel<'a> {
    repository: &'a SemanticRepository,
    symbols: &'a SymbolIndex,
    scopes: &'a ScopeIndex,
    bindings: &'a BindingIndex,
    inheritance: &'a InheritanceIndex,
    rust_files: HashMap<&'a FileId, RustFileScopes<'a>>,
    /// Names of the repository's modules: a `use` path starting with one may name a module of
    /// this crate rather than an external crate.
    module_names: HashSet<&'a str>,
    /// The directory of each Go file, which with its `package` clause is its package:
    /// `billing/ledger`, or `` at the root.
    go_packages: HashMap<&'a FileId, &'a str>,
    /// Go files of an external test package: a `_test.go` file whose clause names the package
    /// beside it with `_test` (`package store_test` in `store/`). No import path names that
    /// package, so it shares its directory with the package an import names but is not it.
    go_external_tests: HashSet<&'a FileId>,
    /// Every Go `_test.go` file, of the external test package or of the package itself.
    go_test_files: HashSet<&'a FileId>,
    /// Every directory holding a Go file of a package an import can name, `` for the root.
    go_package_dirs: HashSet<&'a str>,
    /// Each module a `go.mod` declares, with the manifest's directory (`` at the root).
    go_modules: Vec<(String, &'a str)>,
    /// Each Go type alias by the symbol that declares it.
    go_aliases: HashMap<&'a SymbolId, &'a TypeAliasSite>,
    /// The package each Java file declares (`org.example`), which its directory need not mirror.
    java_packages: HashMap<&'a FileId, &'a str>,
    /// Every Java file, a package declared or not.
    java_files: HashSet<&'a FileId>,
}

struct RustFileScopes<'a> {
    path: &'a Path,
    scopes: Vec<&'a Scope>,
}

impl<'a> RegistryScopeModel<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        files: &'a [File],
        repository: &'a SemanticRepository,
        symbols: &'a SymbolIndex,
        scopes: &'a ScopeIndex,
        bindings: &'a BindingIndex,
        inheritance: &'a InheritanceIndex,
        type_aliases: &'a [TypeAliasSite],
        package_declarations: &'a [PackageDeclarationSite],
    ) -> Self {
        let mut rust_files = files
            .iter()
            .filter(|file| file.language == Language::Rust)
            .map(|file| {
                (
                    &file.id,
                    RustFileScopes {
                        path: file.path.as_path(),
                        scopes: Vec::new(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        for scope in scopes.scopes.values() {
            if let Some(file) = rust_files.get_mut(&scope.file_id) {
                file.scopes.push(scope);
            }
        }
        let module_names = symbols
            .by_id
            .values()
            .filter(|symbol| matches!(symbol.kind, SymbolKind::Module | SymbolKind::Package))
            .map(|symbol| symbol.name.as_str())
            .collect();
        let go_packages = files
            .iter()
            .filter(|file| file.language == Language::Go)
            .filter_map(|file| {
                let dir = file.path.parent().map_or(Some(""), Path::to_str)?;
                Some((&file.id, dir))
            })
            .collect::<HashMap<_, _>>();
        let declared = package_declarations
            .iter()
            .map(|site| (&site.file_id, site.name.as_str()))
            .collect::<HashMap<_, _>>();
        let go_test_files = files
            .iter()
            .filter(|file| {
                file.language == Language::Go
                    && file
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with("_test.go"))
            })
            .map(|file| &file.id)
            .collect::<HashSet<_>>();
        let go_external_tests = go_test_files
            .iter()
            .copied()
            .filter(|file_id| {
                declared
                    .get(file_id)
                    .is_some_and(|package| package.ends_with("_test"))
            })
            .collect::<HashSet<_>>();
        let go_package_dirs = go_packages
            .iter()
            .filter(|(file_id, _)| !go_external_tests.contains(*file_id))
            .map(|(_, dir)| *dir)
            .collect();
        let java_files = files
            .iter()
            .filter(|file| file.language == Language::Java)
            .map(|file| &file.id)
            .collect::<HashSet<_>>();
        let java_packages = java_files
            .iter()
            .filter_map(|&file_id| Some((file_id, *declared.get(file_id)?)))
            .collect();
        let go_modules = repository
            .project
            .roots
            .iter()
            .filter(|root| root.language == Language::Go)
            // The go command ignores `testdata` and `_`- or `.`-prefixed directories, so a
            // `go.mod` there declares no module of the build.
            .filter(|root| {
                !root.path.components().any(|part| {
                    let part = part.as_os_str().to_string_lossy();
                    part == "testdata"
                        || part.starts_with('_')
                        || (part.starts_with('.') && part != ".")
                })
            })
            .filter_map(|root| {
                // `module example.com/app // comment` or `module "example.com/app"`.
                let module = root.package_name.as_deref()?.split_whitespace().next()?;
                let module = module.trim_matches('"');
                let dir = root.path.to_str()?.trim_start_matches("./");
                let dir = if dir == "." {
                    ""
                } else {
                    dir.trim_end_matches('/')
                };
                (!module.is_empty()).then(|| (module.to_string(), dir))
            })
            .collect();
        Self {
            repository,
            symbols,
            scopes,
            bindings,
            inheritance,
            rust_files,
            module_names,
            go_packages,
            go_external_tests,
            go_test_files,
            go_package_dirs,
            go_modules,
            go_aliases: type_aliases
                .iter()
                .map(|site| (&site.symbol_id, site))
                .collect(),
            java_packages,
            java_files,
        }
    }

    /// The directory of the Go package an import path can name that `file_id` belongs to: none
    /// for a file of an external test package.
    fn go_importable_package(&self, file_id: &FileId) -> Option<&'a str> {
        if self.go_external_tests.contains(file_id) {
            return None;
        }
        self.go_packages.get(file_id).copied()
    }

    /// Where a Go import path puts its package. Under a module a `go.mod` declares, the path
    /// below the module is the package's directory below the manifest's, whatever other
    /// directory the path's tail also spells. Outside every declared module it is another
    /// module's package, or the standard library's, unless an indexed `vendor` directory holds
    /// a copy (vendored files are not indexed by default).
    /// With no module declared, the longest directory of the repository the path ends with
    /// stands in (`example.com/app/internal/cli` names `internal/cli`, not a top-level `cli`),
    /// and a path ending with none can only be the root package.
    ///
    /// Two `go.mod` files may declare one module (a test fixture copying the root's): the one
    /// whose directory holds the importing file, `importer_dir`, is the importer's own module,
    /// and otherwise the shallowest one is taken.
    fn go_import_place<'p>(&self, import_path: &'p str, importer_dir: &str) -> ReceiverPlace<'p>
    where
        'a: 'p,
    {
        if !self.go_modules.is_empty() {
            let encloses = |dir: &str| {
                dir.is_empty()
                    || importer_dir
                        .strip_prefix(dir)
                        .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
            };
            let mut modules = self
                .go_modules
                .iter()
                .filter_map(|(module, dir)| {
                    let rest = import_path.strip_prefix(module.as_str())?;
                    let rest = if rest.is_empty() {
                        rest
                    } else {
                        rest.strip_prefix('/')?
                    };
                    Some((module.len(), *dir, rest))
                })
                .collect::<Vec<_>>();
            // The longest module path; then the importer's own, deepest first; then the
            // shallowest.
            modules.sort_by_key(|(len, dir, _)| {
                let own = encloses(dir);
                std::cmp::Reverse((
                    *len,
                    own,
                    if own {
                        dir.len()
                    } else {
                        usize::MAX - dir.len()
                    },
                ))
            });
            // A module whose directory holds no package at that path is passed over for the
            // next that does: a stray `go.mod` must not hide the package the path names.
            let holds_package = |dir: &str, rest: &str| {
                let joined = match (dir.is_empty(), rest.is_empty()) {
                    (true, _) => rest.to_string(),
                    (false, true) => dir.to_string(),
                    (false, false) => format!("{dir}/{rest}"),
                };
                self.go_package_dirs.contains(joined.as_str())
            };
            let module = modules
                .iter()
                .find(|(_, dir, rest)| holds_package(dir, rest))
                .or(modules.first());
            if let Some(&(_, dir, rest)) = module {
                return ReceiverPlace::GoPackage { dir, rest };
            }
            let vendored = self.go_modules.iter().find_map(|(_, dir)| {
                let vendor = if dir.is_empty() {
                    format!("vendor/{import_path}")
                } else {
                    format!("{dir}/vendor/{import_path}")
                };
                self.go_package_dirs.get(vendor.as_str()).copied()
            });
            return match vendored {
                Some(dir) => ReceiverPlace::GoPackage { dir, rest: "" },
                None => ReceiverPlace::Outside,
            };
        }
        let mut rest = import_path;
        loop {
            if let Some(dir) = self.go_package_dirs.get(rest) {
                return ReceiverPlace::GoPackage { dir, rest: "" };
            }
            match rest.split_once('/') {
                Some((_, tail)) => rest = tail,
                None => return ReceiverPlace::GoRoot,
            }
        }
    }

    /// Whether the resolver's verdict that a same-file item is out of reach of `name` at
    /// `scope_id` rests only on imports it models exactly.
    ///
    /// The resolver's rule fails closed: an import it cannot place counts as naming another item,
    /// and a module reaches only what `use super::*` globs bring in. That is right for proving an
    /// edge, but the registry drops a candidate on the verdict, so a path such as
    /// `use super::helpers::make;`, `use self::inner::*;`, `use crate::*;` or a glob inside a
    /// function body would cost a valid call its only caller edge. Only a verdict that no such
    /// import could overturn removes a candidate.
    fn rust_verdict_is_exact(&self, file_id: &FileId, scope_id: &ScopeId, name: &str) -> bool {
        let named = self.file_imports(file_id, name);
        let globs = self.file_imports(file_id, GLOB_IMPORT_LOCAL_NAME);
        let mut current = self.scopes.get(scope_id);
        let mut use_module = None;
        for _ in 0..=self.scopes.scopes.len() {
            let Some(scope) = current else {
                break;
            };
            let here = named
                .iter()
                .filter(|binding| !binding.is_glob && binding.scope_id == scope.id)
                .collect::<Vec<_>>();
            if !here.is_empty() {
                // The nearest explicit import decides, as in the resolver.
                return here.iter().all(|binding| self.import_is_modeled(binding));
            }
            if matches!(scope.kind, ScopeKind::Module | ScopeKind::File) {
                use_module = Some(scope);
                break;
            }
            // A glob inside a block may bring the name in from anywhere.
            if globs.iter().any(|glob| {
                glob.scope_id == scope.id && self.may_name_this_crate(&glob.source_module)
            }) {
                return false;
            }
            current = scope
                .parent_id
                .as_ref()
                .and_then(|parent| self.scopes.get(parent));
        }
        let Some(use_module) = use_module else {
            return false;
        };
        // Every module the resolver's reachability walk visits: only `use super::*` globs are
        // followed, so any other glob of this crate leaves the verdict open.
        let mut pending = vec![use_module];
        let mut seen = HashSet::new();
        while let Some(module) = pending.pop() {
            if !seen.insert(&module.id) {
                continue;
            }
            for glob in globs.iter().filter(|glob| glob.scope_id == module.id) {
                match super_glob_depth(&glob.source_module) {
                    Some(depth) => match self.module_above(module, depth) {
                        Some(parent) => pending.push(parent),
                        None => return false,
                    },
                    None if self.may_name_this_crate(&glob.source_module) => return false,
                    None => {}
                }
            }
        }
        true
    }

    /// Whether the resolver places `binding` exactly: a resolved target, a `self::`/`super::`
    /// path to one item, a `crate::` path (which it never rules out without a target), or an
    /// external crate.
    fn import_is_modeled(&self, binding: &ImportBinding) -> bool {
        if binding.target_symbol.is_some() || binding.target_file.is_some() {
            return true;
        }
        let source = binding.source_module.as_str();
        let Some((path, _item)) = source.rsplit_once("::") else {
            return false;
        };
        let first = path.split("::").next().unwrap_or_default();
        match first {
            "crate" => true,
            "self" if path == "self" => self.relative_item_is_declared(binding, 0),
            "super" if path.split("::").all(|segment| segment == "super") => {
                self.relative_item_is_declared(binding, path.split("::").count())
            }
            "self" | "super" => false,
            _ => !self.may_name_this_crate(source),
        }
    }

    /// Whether the module a one-item `self::x` or `super::x` path names declares `x` itself, with
    /// no import there that could bind it instead. The resolver maps such a path to the item the
    /// module declares; rustc binds it to whatever the module's scope holds, which may be a `use`
    /// of another module's item (`use a::x;` or `pub use a::*;` in the parent).
    fn relative_item_is_declared(&self, binding: &ImportBinding, depth: usize) -> bool {
        let Some((_, item)) = binding.source_module.rsplit_once("::") else {
            return false;
        };
        let Some(start) = self.enclosing_module(&binding.scope_id) else {
            return false;
        };
        let Some(module) = self.module_above(start, depth) else {
            return false;
        };
        let declares = !self
            .symbols
            .lookup_file_scope_name(&binding.file_id, &module.id, item)
            .is_empty();
        let imported = self
            .file_imports(&binding.file_id, item)
            .iter()
            .any(|other| !other.is_glob && other.scope_id == module.id);
        let globbed = self
            .file_imports(&binding.file_id, GLOB_IMPORT_LOCAL_NAME)
            .iter()
            .any(|glob| {
                glob.scope_id == module.id && self.may_name_this_crate(&glob.source_module)
            });
        declares && !imported && !globbed
    }

    /// The nearest module or file scope at or above `scope_id`.
    fn enclosing_module(&self, scope_id: &ScopeId) -> Option<&'a Scope> {
        let mut current = self.scopes.get(scope_id);
        for _ in 0..=self.scopes.scopes.len() {
            let scope = current?;
            if matches!(scope.kind, ScopeKind::Module | ScopeKind::File) {
                return Some(scope);
            }
            current = scope.parent_id.as_ref().and_then(|id| self.scopes.get(id));
        }
        None
    }

    /// Whether `source` may be a path into this crate rather than into an external one.
    fn may_name_this_crate(&self, source: &str) -> bool {
        let first = source.split("::").next().unwrap_or_default();
        matches!(first, "crate" | "self" | "super" | "Self") || self.module_names.contains(first)
    }

    fn file_imports(&self, file_id: &FileId, local_name: &str) -> &'a [ImportBinding] {
        self.repository
            .imports
            .by_file_local_name
            .get(&(file_id.clone(), local_name.to_string()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The module `depth` levels above `module`, when it is in this file.
    fn module_above(&self, module: &'a Scope, depth: usize) -> Option<&'a Scope> {
        let mut current = module;
        for _ in 0..depth {
            if current.kind != ScopeKind::Module {
                return None;
            }
            let mut parent = self.scopes.get(current.parent_id.as_ref()?);
            current = loop {
                let scope = parent?;
                if matches!(scope.kind, ScopeKind::Module | ScopeKind::File) {
                    break scope;
                }
                parent = scope.parent_id.as_ref().and_then(|id| self.scopes.get(id));
            };
        }
        Some(current)
    }

    /// The innermost scope of a Rust file around a bare token, or `None` when the token is not a
    /// bare Rust name or its file has no scopes, in which case scoping rules nothing out.
    fn rust_use_scope(&self, chunk: &CodeChunk, token_use: &TokenUse) -> Option<&'a ScopeId> {
        if chunk.language != Language::Rust || !token_use.bare {
            return None;
        }
        let file = self.rust_files.get(&chunk.file_id)?;
        let position = (
            chunk
                .range
                .start
                .saturating_add(token_use.line)
                .saturating_sub(1),
            token_use.column,
        );
        file.scopes
            .iter()
            .filter(|scope| {
                (scope.range.start_line, scope.range.start_column) <= position
                    && position <= (scope.range.end_line, scope.range.end_column)
            })
            // A nested scope starts no earlier than its parent; of two starting together the
            // shorter is inside the other.
            .max_by_key(|scope| {
                (
                    (scope.range.start_line, scope.range.start_column),
                    std::cmp::Reverse((scope.range.end_line, scope.range.end_column)),
                )
            })
            .map(|scope| &scope.id)
    }

    fn rust_context(&self, file_id: &'a FileId) -> Option<ResolutionContext<'a>> {
        let file = self.rust_files.get(file_id)?;
        Some(ResolutionContext::new(
            file_id,
            file.path,
            None,
            Language::Rust,
            self.repository,
            self.symbols,
            self.scopes,
            self.bindings,
            self.inheritance,
            open_kioku_languages::semantics_for(&Language::Rust)?,
        ))
    }
}

/// Which name-matched items of the use site's own file Rust scoping keeps out of reach. Worked
/// out on the first same-file candidate only, since most tokens match none.
struct ScopeFilter<'m, 'c> {
    model: Option<&'m RegistryScopeModel<'m>>,
    chunk: &'c CodeChunk,
    token_use: &'c TokenUse,
    site: OnceCell<Option<(ResolutionContext<'m>, &'m ScopeId)>>,
    exact: OnceCell<bool>,
    /// Items this use was kept from, so another use of the name on its line does not take them.
    ruled_out: RefCell<Vec<SymbolId>>,
}

impl<'m, 'c> ScopeFilter<'m, 'c> {
    fn new(
        model: Option<&'m RegistryScopeModel<'m>>,
        chunk: &'c CodeChunk,
        token_use: &'c TokenUse,
    ) -> Self {
        Self {
            model,
            chunk,
            token_use,
            site: OnceCell::new(),
            exact: OnceCell::new(),
            ruled_out: RefCell::new(Vec::new()),
        }
    }

    fn admits(&self, symbol: &Symbol) -> bool {
        let token = self.token_use.token.as_str();
        if symbol.file_id != self.chunk.file_id || !symbol_matches_token(symbol, token) {
            return true;
        }
        let site = self.site.get_or_init(|| {
            let model = self.model?;
            let scope_id = model.rust_use_scope(self.chunk, self.token_use)?;
            let (file_id, _) = model.rust_files.get_key_value(&self.chunk.file_id)?;
            Some((model.rust_context(file_id)?, scope_id))
        });
        let Some((ctx, scope_id)) = site else {
            return true;
        };
        let ruled_out = rust_rules_out_same_file_item(ctx, scope_id, token, symbol)
            && self.model.is_some_and(|model| {
                *self.exact.get_or_init(|| {
                    model.rust_verdict_is_exact(&self.chunk.file_id, scope_id, token)
                })
            });
        if ruled_out {
            self.ruled_out.borrow_mut().push(symbol.id.clone());
        }
        !ruled_out
    }
}

/// `super::*` is 1 and `super::super::*` is 2; any other path is `None`.
fn super_glob_depth(source: &str) -> Option<usize> {
    let path = source.strip_suffix("::*")?;
    let segments = path.split("::").collect::<Vec<_>>();
    segments
        .iter()
        .all(|segment| *segment == "super")
        .then_some(segments.len())
}

impl SymbolRegistry {
    pub fn new(symbols: &[Symbol], import_resolutions: &[ImportResolution]) -> Self {
        let mut registry = Self {
            by_id: HashMap::with_capacity(symbols.len()),
            by_qualified_name: HashMap::new(),
            by_simple_name: HashMap::new(),
            by_file: HashMap::new(),
            by_module: HashMap::new(),
            import_resolutions: import_resolutions.to_vec(),
            by_file_imports: HashMap::new(),
            by_file_token: HashMap::new(),
            by_name_suffix: HashMap::new(),
            qualified_name_normalized: HashMap::new(),
            places: HashSet::new(),
            alias_targets: HashMap::new(),
            go_test_files: HashMap::new(),
            go_dirs: HashMap::new(),
            java_packages: HashSet::new(),
            java_files: HashSet::new(),
        };
        for (idx, import) in import_resolutions.iter().enumerate() {
            registry
                .by_file_imports
                .entry(import.import.file_id.clone())
                .or_default()
                .push(idx);
        }
        for symbol in symbols {
            registry.by_id.insert(symbol.id.clone(), symbol.clone());
            registry
                .by_qualified_name
                .entry(symbol.qualified_name.clone())
                .or_default()
                .push(symbol.id.clone());
            registry
                .by_simple_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id.clone());
            registry
                .by_file
                .entry(symbol.file_id.clone())
                .or_default()
                .push(symbol.id.clone());
            let last_segment = symbol
                .qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(&symbol.qualified_name);
            for key in [symbol.name.as_str(), last_segment] {
                let ids = registry
                    .by_file_token
                    .entry((symbol.file_id.clone(), key.to_string()))
                    .or_default();
                if ids.last() != Some(&symbol.id) {
                    ids.push(symbol.id.clone());
                }
            }
            registry
                .by_module
                .entry(module_name(&symbol.qualified_name))
                .or_default()
                .push(symbol.id.clone());
            if let Some((path, _)) = symbol.qualified_name.rsplit_once("::") {
                for segment in path.split("::") {
                    if segment.contains('-') {
                        registry.places.insert(segment.replace('-', "_"));
                    } else if !registry.places.contains(segment) {
                        registry.places.insert(segment.to_string());
                    }
                }
            }
            let suffix = qualified_name_suffix(&symbol.qualified_name);
            registry
                .by_name_suffix
                .entry(suffix)
                .or_default()
                .push(symbol.id.clone());
            if !registry
                .qualified_name_normalized
                .contains_key(&symbol.qualified_name)
            {
                registry.qualified_name_normalized.insert(
                    symbol.qualified_name.clone(),
                    symbol.qualified_name.replace("::", "."),
                );
            }
        }
        registry
    }

    fn resolve(
        &self,
        chunk: &CodeChunk,
        token_use: &TokenUse,
        scope: &ScopeFilter<'_, '_>,
    ) -> Resolution {
        let token = token_use.token.as_str();
        // An item of this file that Rust scoping keeps out of reach is not the target by any
        // strategy: the registry's imports are file-wide, so an import resolved to this file (a
        // `use super::*` beside `use mock_clock::now;`) would offer it again, and so would the
        // name-based fallbacks. See `scoped_resolution` for what its removal may decide.
        // A Go declaration the use cannot reach is ruled out the same way: never the target, and
        // beside another candidate the match stays ambiguous, as for any ruled-out item.
        let admits = |symbol: &Symbol| scope.admits(symbol) && self.go_test_reaches(chunk, symbol);
        if let Some(resolution) = self.resolve_import_target(chunk, token, &admits) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_same_file(chunk, token, &admits) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_same_module(chunk, token, &admits) {
            return resolution;
        }
        let unresolved = |reason: &str| Resolution {
            symbol: None,
            strategy: "unresolved",
            candidates: 0,
            confidence: Confidence::Low,
            ambiguity_reason: Some(reason.into()),
            speculative: true,
        };
        // Past this point only the name links a token to a symbol anywhere in the repository,
        // which the token's place in the code can rule out (#582).
        match token_use.role {
            TokenRole::Name => {}
            TokenRole::Field => {
                // Only a field can be named here; a same-named function is not a candidate.
                let fields = |symbol: &Symbol| symbol.kind == SymbolKind::Field && admits(symbol);
                return self
                    .resolve_unique_project_name(chunk, token, &fields)
                    .unwrap_or_else(|| unresolved("no field candidate matched a field name"));
            }
            TokenRole::Attribute if chunk.language == Language::Java => {
                // A Java annotation names a type: `@Retries.RetryRaw` may name the repository's
                // `Retries`, never a method or field of that name.
                let types = |symbol: &Symbol| {
                    matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
                        && admits(symbol)
                };
                return self
                    .resolve_unique_project_name(chunk, token, &types)
                    .unwrap_or_else(|| {
                        unresolved(TokenRole::Attribute.withheld_reason().unwrap_or_default())
                    });
            }
            TokenRole::Member => {
                // A receiver that names where the candidate is defined is evidence a bare
                // member lacks: Go's `ledger.NewEntry`, Java's `Constants.ACCESS_KEY`.
                // A lowercase Java receiver is a variable, whose name may match a package's.
                let receiver = token_use.receiver.as_deref().filter(|receiver| {
                    chunk.language != Language::Java || receiver.starts_with(char::is_uppercase)
                });
                let Some(receiver) = receiver else {
                    return unresolved(TokenRole::Member.withheld_reason().unwrap_or_default());
                };
                let place = self.receiver_place(chunk, receiver, scope.model);
                if matches!(place, ReceiverPlace::Outside) {
                    return unresolved(
                        "the member's receiver is imported from outside the repository",
                    );
                }
                // An import names a package by its directory, never an external test package
                // beside it.
                let go_package = |symbol: &Symbol| {
                    scope
                        .model
                        .and_then(|model| model.go_importable_package(&symbol.file_id))
                };
                let owned = |symbol: &Symbol| {
                    admits(symbol)
                        && match place {
                            ReceiverPlace::Named => segment_locates(self, receiver, symbol),
                            ReceiverPlace::Outside => false,
                            ReceiverPlace::GoPackage { dir, rest } => go_package(symbol)
                                .is_some_and(|package| joined_path_is(package, dir, rest)),
                            ReceiverPlace::GoRoot => {
                                go_package(symbol) == Some("")
                                    && segment_locates(self, receiver, symbol)
                            }
                        }
                };
                return self
                    .resolve_unique_project_name(chunk, token, &owned)
                    .unwrap_or_else(|| {
                        unresolved(match place {
                            ReceiverPlace::GoPackage { .. } | ReceiverPlace::GoRoot => {
                                "no registry candidate is in the package the receiver's import names"
                            }
                            _ => TokenRole::Member.withheld_reason().unwrap_or_default(),
                        })
                    });
            }
            role => {
                return unresolved(role.withheld_reason().unwrap_or_default());
            }
        }
        // So can a path or an import that spells where the name comes from: in
        // `std::mem::take(..)`, beside `use anyhow::Result;` or in `ScopeKind::File`, the one
        // `take`, `Result` or `File` of the repository is not the target.
        let origin = self.origin(chunk, token_use, scope.model);
        let located = |symbol: &Symbol| {
            admits(symbol)
                && match origin {
                    Origin::Anywhere => true,
                    Origin::Outside => false,
                    Origin::MemberOf(owner) => segment_locates(self, owner, symbol),
                    Origin::StaticMemberOf(bindings) => {
                        let package = scope
                            .model
                            .and_then(|model| model.java_packages.get(&symbol.file_id).copied())
                            .unwrap_or_default();
                        bindings.iter().any(|binding| {
                            !self.import_leads_outside(binding)
                                && static_import_owner(binding).is_some_and(|owner| {
                                    owner_locates(self, owner, package, symbol)
                                })
                        })
                    }
                }
        };
        if let Some(resolution) = self.resolve_unique_project_name(chunk, token, &located) {
            return resolution;
        }
        if let Some(resolution) =
            self.resolve_suffix_with_import_reachability(chunk, token, &located)
        {
            return resolution;
        }
        self.resolve_fuzzy(chunk, token, &located)
            .unwrap_or_else(|| {
                unresolved(match origin {
                    Origin::Anywhere => "no registry candidate matched",
                    Origin::Outside => "the name's path or import leads outside the repository",
                    Origin::MemberOf(_) => {
                        "no registry candidate belongs to the type its path names"
                    }
                    Origin::StaticMemberOf(_) => {
                        "no registry candidate belongs to the class its static import names"
                    }
                })
            })
    }

    /// Where the path or import spelling a token says its target is.
    fn origin<'t>(
        &self,
        chunk: &CodeChunk,
        token_use: &'t TokenUse,
        model: Option<&RegistryScopeModel<'t>>,
    ) -> Origin<'t> {
        if let Some(qualifier) = token_use.qualifier.as_deref() {
            if matches!(qualifier, "crate" | "self" | "super" | "Self") || self.is_module(qualifier)
            {
                // A module may re-export the item from anywhere.
                return Origin::Anywhere;
            }
            let names_type = self.by_simple_name.get(qualifier).is_some_and(|ids| {
                ids.iter()
                    .filter_map(|id| self.by_id.get(id))
                    .any(|symbol| {
                        matches!(
                            symbol.kind,
                            SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
                        )
                    })
            });
            return if names_type {
                Origin::MemberOf(qualifier)
            } else {
                Origin::Outside
            };
        }
        // Most tokens name nothing any strategy below could match; they need no import lookup.
        let token = token_use.token.as_str();
        if self.by_simple_name.len() > MAX_SIMPLE_NAMES_FOR_FUZZY
            && !self.by_simple_name.contains_key(token)
            && !self.by_name_suffix.contains_key(token)
        {
            return Origin::Anywhere;
        }
        // The file's own import of the name, by the name it binds (`use std::fs::File as FsFile;`
        // binds `FsFile`, not `File`), when the resolver's import model is at hand.
        let Some(model) = model else {
            return Origin::Anywhere;
        };
        let bindings = model.file_imports(&chunk.file_id, token);
        if bindings.is_empty() {
            return Origin::Anywhere;
        }
        if bindings
            .iter()
            .all(|binding| self.import_leads_outside(binding))
        {
            return Origin::Outside;
        }
        // A static import under the repository's package root may still import a library's
        // class (`import static org.example.vendor.Keys.ACCESS_KEY;`): the class it names must
        // be the one the candidate belongs to, as for a path through a type.
        if bindings.iter().all(|binding| {
            self.import_leads_outside(binding) || static_import_owner(binding).is_some()
        }) {
            Origin::StaticMemberOf(bindings)
        } else {
            Origin::Anywhere
        }
    }

    /// Whether the resolver left `binding` unplaced and its path starts outside the repository:
    /// `use anyhow::Result;`, `import static com.vendor.Keys.ACCESS_KEY;`.
    fn import_leads_outside(&self, binding: &ImportBinding) -> bool {
        // A Java static import keeps its keyword: `static org.x.Constants.NAME`.
        let source = binding.source_module.as_str();
        let source = source
            .strip_prefix("static ")
            .unwrap_or(source)
            .trim_start();
        let root = source
            .split([':', '.', '/'])
            .find(|segment| !segment.is_empty())
            .unwrap_or_default();
        !binding.is_glob
            && binding.target_symbol.is_none()
            && binding.target_file.is_none()
            && !source.starts_with('.')
            && !matches!(root, "crate" | "self" | "super" | "Self")
            && !self.is_module(root)
            && !self.by_simple_name.contains_key(root)
            && !self.java_import_is_declared_inside(binding, source)
    }

    /// Where a member's receiver says the member is. A receiver the file imports is where its
    /// import leads: a library's package is not the repository's same-named one, and in Go the
    /// whole import path, not its last segment, names the package directory.
    fn receiver_place<'t>(
        &self,
        chunk: &CodeChunk,
        receiver: &str,
        model: Option<&RegistryScopeModel<'t>>,
    ) -> ReceiverPlace<'t> {
        let Some(model) = model else {
            return ReceiverPlace::Named;
        };
        if chunk.language == Language::Go {
            return self.go_receiver_place(&chunk.file_id, receiver, model);
        }
        let bindings = model.file_imports(&chunk.file_id, receiver);
        let outside = !bindings.is_empty()
            && bindings
                .iter()
                .all(|binding| self.import_leads_outside(binding));
        if outside {
            ReceiverPlace::Outside
        } else {
            ReceiverPlace::Named
        }
    }

    /// Where a Go receiver `file_id` names puts its package: by the whole path of the import
    /// binding it, or, with no single import binding it, by its name alone.
    fn go_receiver_place<'t>(
        &self,
        file_id: &FileId,
        receiver: &str,
        model: &RegistryScopeModel<'t>,
    ) -> ReceiverPlace<'t> {
        let [binding] = model.file_imports(file_id, receiver) else {
            return ReceiverPlace::Named;
        };
        let path = binding.source_module.as_str();
        // With no module declared, the directory the path's tail spells is only a guess, which
        // the resolver's standard-library verdict overrides. Under a declared module the
        // module decides: the resolver calls any path without a dot the standard library's,
        // `myapp/internal/store` of `module myapp` included.
        let standard_library = model.go_modules.is_empty()
            && matches!(
                self.import_status(file_id, path),
                Some(ResolutionStatus::Builtin)
            );
        if standard_library {
            ReceiverPlace::Outside
        } else {
            let importer_dir = model.go_packages.get(file_id).copied().unwrap_or_default();
            model.go_import_place(path, importer_dir)
        }
    }

    /// Reads what the packages files declare says about which names reach which symbols.
    fn read_declared_packages(&mut self, model: &RegistryScopeModel<'_>) {
        self.go_test_files = model
            .go_test_files
            .iter()
            .filter_map(|&file_id| {
                let dir = model.go_packages.get(file_id)?;
                let test = GoTestFile {
                    dir: dir.to_string(),
                    external: model.go_external_tests.contains(file_id),
                };
                Some((file_id.clone(), test))
            })
            .collect();
        if !self.go_test_files.is_empty() {
            self.go_dirs = model
                .go_packages
                .iter()
                .map(|(&file_id, dir)| (file_id.clone(), dir.to_string()))
                .collect();
        }
        self.java_packages = model
            .java_packages
            .values()
            .filter(|package| !package.is_empty())
            .map(|package| package.to_string())
            .collect();
        self.java_files = model
            .java_files
            .iter()
            .map(|&file_id| file_id.clone())
            .collect();
    }

    /// Whether `binding`, an import of a Java file, names something inside a package the
    /// repository declares: `org.example.Constants` or `static org.example.Constants.KEY` when a
    /// file declares `package org.example;`, whatever directory holds it. Only whole leading
    /// segments count, so `org.apache.commons.Widget` is not inside `org.example`, and another
    /// language's import (Python's `from io import StringIO` beside a Java `package io.acme;`)
    /// is never read against a Java package.
    fn java_import_is_declared_inside(&self, binding: &ImportBinding, source: &str) -> bool {
        if self.java_packages.is_empty() || !self.java_files.contains(&binding.file_id) {
            return false;
        }
        source
            .match_indices('.')
            .any(|(end, _)| self.java_packages.contains(&source[..end]))
    }

    /// Whether a use in `chunk` can name `symbol` as far as Go test files say: a declaration of a
    /// `_test.go` file is compiled only into the tests of its own directory's package, so a use in
    /// another directory cannot name it, nor a use outside the external test package
    /// (`package store_test`) name one of that package's (#620).
    fn go_test_reaches(&self, chunk: &CodeChunk, symbol: &Symbol) -> bool {
        let Some(test) = self.go_test_files.get(&symbol.file_id) else {
            return true;
        };
        let external = self
            .go_test_files
            .get(&chunk.file_id)
            .is_some_and(|own| own.external);
        self.go_dirs.get(&chunk.file_id) == Some(&test.dir) && (external || !test.external)
    }

    /// Reads every Go type alias of `model` to the repository type it stands for.
    fn place_go_aliases(&mut self, model: &RegistryScopeModel<'_>) {
        let mut targets = HashMap::with_capacity(model.go_aliases.len());
        for (&alias_id, &site) in &model.go_aliases {
            let written = self
                .by_id
                .get(alias_id)
                .and_then(|alias| alias.signature.as_deref())
                .and_then(|signature| signature.split_once(" = "))
                .map_or_else(String::new, |(_, written)| written.to_string());
            let mut site = site;
            let mut visited = vec![alias_id];
            let target = loop {
                let Some(next) = self.go_alias_step(site, model) else {
                    break AliasTarget::Unplaced(written);
                };
                match model.go_aliases.get(&next) {
                    None => break AliasTarget::Type(next.clone()),
                    // An alias of an alias: its own file's imports place the next step.
                    Some(&next_site)
                        if !visited.contains(&&next) && visited.len() < MAX_ALIAS_HOPS =>
                    {
                        visited.push(&next_site.symbol_id);
                        site = next_site;
                    }
                    Some(_) => break AliasTarget::Unplaced(written),
                }
            };
            targets.insert(alias_id.clone(), target);
        }
        self.alias_targets = targets;
    }

    /// The one type declaration of the package `site`'s target names, under the target's name:
    /// the alias's own package for an unqualified name, else the package its qualifier's import
    /// leads to.
    fn go_alias_step(
        &self,
        site: &TypeAliasSite,
        model: &RegistryScopeModel<'_>,
    ) -> Option<SymbolId> {
        let name = site.target_name.as_deref()?;
        let alias = self.by_id.get(&site.symbol_id)?;
        let alias_package = model.go_packages.get(&alias.file_id).copied()?;
        let alias_external = model.go_external_tests.contains(&alias.file_id);
        let place = match site.target_package.as_deref() {
            None => ReceiverPlace::GoPackage {
                dir: alias_package,
                rest: "",
            },
            Some(qualifier) => self.go_receiver_place(&alias.file_id, qualifier, model),
        };
        // An unqualified name is of the alias's own package, the directory and the clause; a
        // qualified one of the package an import names, which is never an external test package.
        let in_place = |file_id: &FileId, package: &str| {
            let external = model.go_external_tests.contains(file_id);
            match place {
                ReceiverPlace::GoPackage { dir, rest } => {
                    joined_path_is(package, dir, rest)
                        && external == (site.target_package.is_none() && alias_external)
                }
                ReceiverPlace::GoRoot => package.is_empty() && !external,
                // A qualifier no single import binds places nothing.
                ReceiverPlace::Named | ReceiverPlace::Outside => false,
            }
        };
        let mut candidates = self
            .by_simple_name
            .get(name)?
            .iter()
            .filter(|id| **id != site.symbol_id)
            .filter_map(|id| self.by_id.get(id))
            .filter(|symbol| {
                symbol.language == Language::Go
                    && matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
                    // A type declared inside a function is not the package's.
                    && symbol.parent_symbol_id.is_none()
                    && model
                        .go_packages
                        .get(&symbol.file_id)
                        .is_some_and(|package| in_place(&symbol.file_id, package))
            });
        let target = candidates.next()?;
        // Two declarations of the name (one per build constraint) leave the target open.
        candidates.next().is_none().then(|| target.id.clone())
    }

    /// `symbol`, or the repository type it stands for when it is a Go type alias whose target
    /// the registry placed.
    fn through_alias(&self, symbol: Symbol) -> Symbol {
        match self.alias_targets.get(&symbol.id) {
            Some(AliasTarget::Type(target)) => self.by_id.get(target).cloned().unwrap_or(symbol),
            _ => symbol,
        }
    }

    /// `resolution_from_candidates` over the candidates `admits` keeps, when it keeps all or none.
    ///
    /// Ruling some candidates out does not show that the name means the rest: a `mod tests`
    /// helper out of reach of a `let path = dir.path()` says nothing about the `path` method that
    /// remains. A strategy that matched a ruled-out item beside others stays ambiguous, as it was.
    /// With no survivor the strategy matched nothing and the next one runs.
    ///
    /// `admits` judges a Go type alias where it is declared, and an alias it rules out is not
    /// kept as a candidate: an alias declares no type of its own, so a use whose receiver names
    /// another package cannot mean it, and it must not make ambiguous a match that was unique
    /// before aliases were indexed (`store.Entry` beside another package's
    /// `type Entry = Record`). The candidates are then read through their aliases:
    /// an alias and the type it stands for are one candidate, and a match of an alias is a match
    /// of its target. An alias matched alone whose target could not be placed resolves to
    /// nothing, with a caveat naming the alias.
    fn scoped_resolution(
        &self,
        strategy: &'static str,
        candidates: Vec<Symbol>,
        admits: &dyn Fn(&Symbol) -> bool,
        confidence: Confidence,
        speculative: bool,
    ) -> Option<Resolution> {
        let (kept, mut dropped): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|symbol| admits(symbol));
        dropped.retain(|symbol| !self.alias_targets.contains_key(&symbol.id));
        let candidates = if kept.is_empty() || dropped.is_empty() {
            kept
        } else {
            kept.into_iter().chain(dropped).collect()
        };
        let candidates = candidates
            .into_iter()
            .map(|symbol| self.through_alias(symbol))
            .collect();
        let resolution = resolution_from_candidates(strategy, candidates, confidence, speculative)?;
        let unplaced = resolution.symbol.as_ref().and_then(|symbol| {
            match self.alias_targets.get(&symbol.id) {
                Some(AliasTarget::Unplaced(written)) => Some((symbol, written)),
                _ => None,
            }
        });
        let Some((alias, written)) = unplaced else {
            return Some(resolution);
        };
        let written = if written.is_empty() {
            String::new()
        } else {
            format!(" `{written}`")
        };
        Some(Resolution {
            ambiguity_reason: Some(format!(
                "`{}` is a Go type alias of{written}, which the registry could not place at one repository type",
                alias.qualified_name
            )),
            symbol: None,
            candidates: 1,
            confidence: Confidence::Low,
            speculative: true,
            strategy,
        })
    }

    /// How the resolver placed the import of `path` in `file_id`, when it saw one.
    fn import_status(&self, file_id: &FileId, path: &str) -> Option<&ResolutionStatus> {
        self.by_file_imports
            .get(file_id)?
            .iter()
            .map(|&idx| &self.import_resolutions[idx])
            .find(|resolution| resolution.import.imported == path)
            .map(|resolution| &resolution.status)
    }

    /// Whether `name` is a place in the repository a path can go through: a directory, file or
    /// module of some symbol's qualified name (a crate's `-` spelled `_`), or a module symbol.
    fn is_module(&self, name: &str) -> bool {
        self.places.contains(name)
            || self.by_simple_name.get(name).is_some_and(|ids| {
                ids.iter()
                    .filter_map(|id| self.by_id.get(id))
                    .any(|symbol| matches!(symbol.kind, SymbolKind::Module | SymbolKind::Package))
            })
    }

    fn resolve_import_target(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        let mut candidates = Vec::new();
        let indices = self.by_file_imports.get(&chunk.file_id);
        for &idx in indices.into_iter().flatten() {
            let import = &self.import_resolutions[idx];
            if !matches!(import.status, ResolutionStatus::Resolved) {
                continue;
            }
            if let Some(symbol_id) = &import.target_symbol {
                if let Some(symbol) = self.by_id.get(symbol_id) {
                    if symbol_matches_token(symbol, token) || import_mentions_token(import, token) {
                        candidates.push(symbol.clone());
                    }
                }
            } else if let Some(file_id) = &import.target_file {
                // A token spelling a path can match a qualified-name suffix of several segments,
                // which the per-name index does not key.
                let ids = if token.contains(':') {
                    self.by_file.get(file_id)
                } else {
                    self.by_file_token
                        .get(&(file_id.clone(), token.to_string()))
                };
                candidates.extend(
                    ids.into_iter()
                        .flatten()
                        .filter_map(|id| self.by_id.get(id))
                        .filter(|symbol| symbol_matches_token(symbol, token))
                        .cloned(),
                );
            }
        }
        self.scoped_resolution("direct-import", candidates, admits, Confidence::High, false)
    }

    fn resolve_same_file(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        let candidates = self
            .by_file
            .get(&chunk.file_id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.by_id.get(id))
            .filter(|symbol| symbol_matches_token(symbol, token))
            .cloned()
            .collect::<Vec<_>>();
        self.scoped_resolution("same-file", candidates, admits, Confidence::High, false)
    }

    fn resolve_same_module(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        let current = chunk
            .symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .map(|symbol| module_name(&symbol.qualified_name))?;
        let candidates = self
            .by_module
            .get(&current)
            .into_iter()
            .flatten()
            .filter_map(|id| self.by_id.get(id))
            .filter(|symbol| symbol_matches_token(symbol, token))
            .cloned()
            .collect::<Vec<_>>();
        self.scoped_resolution(
            "same-module",
            in_language_family(chunk, candidates),
            admits,
            Confidence::Medium,
            false,
        )
    }

    fn resolve_unique_project_name(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        let candidates = self.by_simple_name.get(token)?;
        if candidates.len() > COMMON_NAME_CAP {
            return Some(Resolution {
                symbol: None,
                strategy: "common-name-cap",
                candidates: candidates.len(),
                confidence: Confidence::Low,
                ambiguity_reason: Some(format!(
                    "common name `{token}` has {} candidates; resolver cap is {COMMON_NAME_CAP}",
                    candidates.len()
                )),
                speculative: true,
            });
        }
        let symbols = candidates
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .cloned()
            .collect::<Vec<_>>();
        self.scoped_resolution(
            "unique-project-name",
            in_language_family(chunk, symbols),
            admits,
            Confidence::Medium,
            true,
        )
    }

    fn resolve_suffix_with_import_reachability(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        let import_indices = self.by_file_imports.get(&chunk.file_id);
        let imported_suffixes = import_indices
            .into_iter()
            .flatten()
            .map(|&idx| self.import_resolutions[idx].import.imported.as_str())
            .collect::<Vec<_>>();
        let suffix_ids = self.by_name_suffix.get(token);
        let candidates = suffix_ids
            .into_iter()
            .flatten()
            .filter_map(|id| {
                let symbol = self.by_id.get(id)?;
                let normalized = self.qualified_name_normalized.get(&symbol.qualified_name)?;
                if imported_suffixes
                    .iter()
                    .any(|imported| normalized.ends_with(*imported))
                {
                    Some(symbol.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        self.scoped_resolution(
            "suffix-import-reachability",
            in_language_family(chunk, candidates),
            admits,
            Confidence::Low,
            true,
        )
    }

    fn resolve_fuzzy(
        &self,
        chunk: &CodeChunk,
        token: &str,
        admits: &dyn Fn(&Symbol) -> bool,
    ) -> Option<Resolution> {
        if token.len() <= 3 || self.by_simple_name.len() > MAX_SIMPLE_NAMES_FOR_FUZZY {
            return None;
        }
        let candidates = self
            .by_simple_name
            .iter()
            .filter(|(name, _)| {
                name.len() > 3 && (name.contains(token) || token.contains(name.as_str()))
            })
            .flat_map(|(_, ids)| ids)
            .filter_map(|id| self.by_id.get(id))
            .take(COMMON_NAME_CAP + 1)
            .cloned()
            .collect::<Vec<_>>();
        self.scoped_resolution(
            "fuzzy-fallback",
            in_language_family(chunk, candidates),
            admits,
            Confidence::Low,
            true,
        )
    }
}

pub fn resolve_symbol_edges(
    chunks: &[CodeChunk],
    symbols: &[Symbol],
    import_resolutions: &[ImportResolution],
    scip_available: bool,
    scope_model: Option<&RegistryScopeModel<'_>>,
) -> RegistryReport {
    let mut registry = SymbolRegistry::new(symbols, import_resolutions);
    if let Some(model) = scope_model {
        registry.read_declared_packages(model);
        registry.place_go_aliases(model);
    }
    // Scoped to this run: dropped with the report, so nothing accumulates in a
    // long-lived process. Shared across the rayon workers below.
    let interner = StringInterner::new();

    // The collect keeps chunk order whatever order the workers finish in. Workers return only
    // how many tokens stayed unresolved, not the names.
    let per_chunk_results: Vec<_> = chunks
        .par_iter()
        .map(|chunk| {
            let resolution =
                resolve_chunk(&registry, chunk, scip_available, scope_model, &interner);
            (
                resolution.facts,
                resolution.notes,
                resolution.unresolved.len(),
            )
        })
        .collect();

    let mut report = RegistryReport::default();
    let mut unresolved_budget = MAX_UNRESOLVED_NOTES;
    let mut unresolved_total = 0usize;
    for (chunk, (facts, notes, unresolved_count)) in chunks.iter().zip(per_chunk_results) {
        unresolved_total += unresolved_count;
        report.analysis_facts.extend(facts);
        report.quality_notes.extend(notes);
        // The unresolved-note cap is spent in chunk order. A counter shared by the workers let
        // whichever chunks finished first claim it, so one build reported different unresolved
        // names on every run (#461). Only the chunks that fill the cap are resolved again to
        // recover their token names; holding every chunk's names would cost memory in
        // proportion to the repository.
        if unresolved_budget == 0 || unresolved_count == 0 {
            continue;
        }
        let tokens =
            resolve_chunk(&registry, chunk, scip_available, scope_model, &interner).unresolved;
        for token in tokens.into_iter().take(unresolved_budget) {
            report.quality_notes.push(QualityNote::new(
                QualityNoteKind::SymbolRegistryUnresolved,
                format!("symbol registry unresolved `{token}` in chunk {}", chunk.id),
            ));
            unresolved_budget -= 1;
        }
    }
    // Without this the cap reads as a repository fact: 64 unresolved names and ten thousand
    // produce the same list. The names beyond the cap are not carried, but their number is.
    if unresolved_total > MAX_UNRESOLVED_NOTES {
        report.quality_notes.push(QualityNote::new(
            QualityNoteKind::SymbolRegistryUnresolved,
            format!(
                "symbol registry unresolved cap is {MAX_UNRESOLVED_NOTES}; {} more unresolved name(s) not listed ({unresolved_total} total)",
                unresolved_total - MAX_UNRESOLVED_NOTES
            ),
        ));
    }

    report.quality_notes.sort();
    report.quality_notes.dedup();
    report.analysis_facts.sort_by(|a, b| a.id.cmp(&b.id));
    report.analysis_facts.dedup_by(|a, b| a.id == b.id);
    report
}

struct ChunkResolution {
    facts: Vec<AnalysisFact>,
    notes: Vec<QualityNote>,
    /// Tokens no strategy resolved, each once, in order of first use.
    unresolved: Vec<String>,
}

fn resolve_chunk(
    registry: &SymbolRegistry,
    chunk: &CodeChunk,
    scip_available: bool,
    scope_model: Option<&RegistryScopeModel<'_>>,
    interner: &StringInterner,
) -> ChunkResolution {
    let mut facts = Vec::new();
    let mut notes = Vec::new();
    let mut unresolved = Vec::new();
    let own_name = chunk
        .symbol_id
        .as_ref()
        .and_then(|id| registry.by_id.get(id))
        .map(|symbol| symbol.name.as_str());
    let uses = token_uses(&chunk.text, &chunk.language)
        .into_iter()
        .take(MAX_TOKENS_PER_CHUNK)
        .filter(|token_use| own_name != Some(token_use.token.as_str()))
        .collect::<Vec<_>>();
    // Items Rust scoping kept from a bare use of a name, per line and whatever the edge type: in
    // `let path = dir.path();` the bare `path` is a local, and the member use must not bring back
    // the item it was ruled out from as the target of `dir.path()`. Every use on the line is
    // resolved before any is emitted, so the rule holds when the member use comes first.
    let mut ruled_out = HashSet::new();
    let resolutions = uses
        .iter()
        .map(|token_use| {
            let scope = ScopeFilter::new(scope_model, chunk, token_use);
            let resolution = registry.resolve(chunk, token_use, &scope);
            for id in scope.ruled_out.take() {
                ruled_out.insert((token_use.token.clone(), token_use.line, id));
            }
            resolution
        })
        .collect::<Vec<_>>();

    let kept = uses
        .into_iter()
        .zip(resolutions)
        .filter(|(token_use, resolution)| {
            !resolution.symbol.as_ref().is_some_and(|symbol| {
                ruled_out.contains(&(token_use.token.clone(), token_use.line, symbol.id.clone()))
            })
        })
        .collect::<Vec<_>>();
    // One edge per token, line and target, and a call takes it over a reference whichever use
    // the line spells first: in `let path = dir.path();` the bare `path` used to take the slot
    // and hide the `CALLS` edge of `dir.path()`, while `dir.path(); path` kept the call (#534).
    let called = kept
        .iter()
        .filter(|(token_use, _)| token_use.is_call)
        .filter_map(|(token_use, resolution)| {
            let symbol = resolution.symbol.as_ref()?;
            Some((
                token_use.token.as_str(),
                token_use.line,
                symbol.id.0.as_str(),
            ))
        })
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut emitted = Vec::with_capacity(kept.len());
    for (token_use, resolution) in &kept {
        let resolved_id = resolution
            .symbol
            .as_ref()
            .map(|symbol| symbol.id.0.as_str())
            .unwrap_or("<unresolved>");
        let key = (token_use.token.as_str(), token_use.line, resolved_id);
        if (!token_use.is_call && called.contains(&key)) || !seen.insert(key) {
            continue;
        }
        emitted.push((token_use, resolution));
    }

    for (token_use, resolution) in emitted {
        if let Some(note) = quality_note(&token_use.token, resolution) {
            notes.push(note);
        }
        if let Some(fact) =
            fact_for_resolution(chunk, token_use, resolution, scip_available, interner)
        {
            facts.push(fact);
        } else if !unresolved.contains(&token_use.token) {
            unresolved.push(token_use.token.clone());
        }
    }
    ChunkResolution {
        facts,
        notes,
        unresolved,
    }
}

fn resolution_from_candidates(
    strategy: &'static str,
    mut candidates: Vec<Symbol>,
    confidence: Confidence,
    speculative: bool,
) -> Option<Resolution> {
    candidates.sort_by(|left, right| {
        symbol_rank(left)
            .cmp(&symbol_rank(right))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            // Same-named symbols stay adjacent by id, so `dedup_by` below removes every repeat
            // whatever order the registry lists them in.
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    candidates.dedup_by(|a, b| a.id == b.id);
    match candidates.len() {
        0 => None,
        1 => Some(Resolution {
            symbol: candidates.pop(),
            strategy,
            candidates: 1,
            confidence,
            ambiguity_reason: None,
            speculative,
        }),
        count => Some(Resolution {
            symbol: None,
            strategy,
            candidates: count,
            confidence: Confidence::Low,
            ambiguity_reason: Some(format!("{count} candidates matched via {strategy}")),
            speculative: true,
        }),
    }
}

fn fact_for_resolution(
    chunk: &CodeChunk,
    token_use: &TokenUse,
    resolution: &Resolution,
    scip_available: bool,
    interner: &StringInterner,
) -> Option<AnalysisFact> {
    let symbol = resolution.symbol.as_ref()?;
    let edge_type = if token_use.is_call {
        GraphEdgeType::Calls
    } else {
        GraphEdgeType::References
    };
    let mut message = format!(
        "symbol registry resolved `{}` to `{}` via {}; candidates={}; scip_available={}; speculative={}",
        token_use.token,
        symbol.qualified_name,
        resolution.strategy,
        resolution.candidates,
        scip_available,
        resolution.speculative
    );
    if let Some(reason) = &resolution.ambiguity_reason {
        message.push_str("; ambiguity: ");
        message.push_str(reason);
    }
    // Interning supersedes compact_message here: `Arc::from(String)` already
    // right-sizes the buffer, and repeated messages collapse to one allocation.
    let message = interner.intern(message);
    Some(AnalysisFact {
        id: identity::stable_hash(&format!(
            "symbol-registry:{}:{}:{}:{}",
            chunk.id, token_use.token, token_use.line, symbol.id.0
        )),
        file_id: chunk.file_id.clone(),
        symbol_id: chunk.symbol_id.clone(),
        target: symbol.qualified_name.clone(),
        target_kind: graph_node_type(symbol),
        edge_type,
        range: Some(open_kioku_core::LineRange::single(
            chunk
                .range
                .start
                .saturating_add(token_use.line)
                .saturating_sub(1),
        )),
        confidence: resolution.confidence,
        source: interner.intern(format!(
            "open-kioku-symbol-registry/{}",
            resolution.strategy
        )),
        source_type: EvidenceSourceType::StaticAnalysis,
        message,
    })
}

fn quality_note(token: &str, resolution: &Resolution) -> Option<QualityNote> {
    resolution.ambiguity_reason.as_ref().map(|reason| {
        QualityNote::new(
            QualityNoteKind::SymbolRegistryCaveat,
            format!(
                "symbol registry caveat for `{token}` via {}: {reason}",
                resolution.strategy
            ),
        )
    })
}

fn token_uses(text: &str, language: &Language) -> Vec<TokenUse> {
    let mut uses = Vec::new();
    let mut lexer = CodeLexer::new(language);
    let mut context = TokenContext::new(language);
    for (line_index, line) in text.lines().enumerate() {
        context.start_line(line);
        let mut previous_end = None;
        for span in lexer.code_spans(line) {
            if previous_end.is_some_and(|end| end < span.start) {
                context.literal();
            }
            previous_end = Some(span.end);
            let mut token: Option<(usize, usize)> = None;
            for (offset, ch) in line[span.clone()].char_indices() {
                let idx = span.start + offset;
                if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                    let start = token.map_or(idx, |(start, _)| start);
                    token = Some((start, idx + ch.len_utf8()));
                    continue;
                }
                if let Some((start, end)) = token.take() {
                    push_code_token(
                        &mut uses,
                        &mut context,
                        language,
                        line,
                        start..end,
                        line_index,
                    );
                }
                context.punct(line, idx, ch);
            }
            if let Some((start, end)) = token {
                push_code_token(
                    &mut uses,
                    &mut context,
                    language,
                    line,
                    start..end,
                    line_index,
                );
            }
        }
    }
    context.mark_locals(&mut uses);
    uses
}

/// A code token, unless it is a string prefix such as Rust's `br` or Python's `rb`, which reads
/// as a name beside the literal it opens.
fn push_code_token(
    uses: &mut Vec<TokenUse>,
    context: &mut TokenContext,
    language: &Language,
    line: &str,
    token: Range<usize>,
    line_index: usize,
) {
    let text = &line[token.clone()];
    let opens_literal = matches!(language, Language::Rust | Language::Python)
        && line[token.end..].starts_with(['"', '\''])
        && text.len() <= 2
        && text.chars().all(|ch| "bBrRfFuUcC".contains(ch));
    if opens_literal {
        return;
    }
    let role = context.word(line, token.clone(), line_index);
    push_token_use(uses, language, line, token, line_index, role);
}

#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Rust,
    /// JavaScript and TypeScript.
    Script,
    Python,
    Java,
    Go,
    Other,
}

#[derive(Clone, Copy, PartialEq)]
enum Bracket {
    Paren,
    Square,
    /// The `[` of a Rust `#[` or `#![` attribute.
    Attribute,
    /// A `{`; `fields` when it opens a Rust struct literal, pattern or item body, whose entries
    /// are field or variant names.
    Brace {
        fields: bool,
    },
}

/// The few kinds of word the words after them depend on.
#[derive(Clone, Copy, PartialEq)]
enum Word {
    Mut,
    Move,
    As,
    Var,
    /// Java: a capitalized name or primitive type, before a declared name.
    Type,
    Other,
}

impl Word {
    fn of(text: &str) -> Self {
        match text {
            "mut" => Word::Mut,
            "move" => Word::Move,
            "as" => Word::As,
            "var" => Word::Var,
            "int" | "long" | "short" | "byte" | "char" | "boolean" | "float" | "double" => {
                Word::Type
            }
            _ if text.starts_with(char::is_uppercase) => Word::Type,
            _ => Word::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Annotation {
    None,
    /// After `@` or a `.` of its path: the next word names the decorator or annotation.
    Name,
    /// After a word of the path: a `.` continues it.
    Dot,
}

/// A lexical reading of where each token stands, carried across a chunk's lines: open brackets,
/// the punctuation just before a word, attribute and annotation paths, and the local bindings
/// the chunk shows (#582). Like `CodeLexer`, it is not a parse. It reads only what a line spells
/// plainly and leaves a token a `Name` wherever it is unsure, which keeps the registry's earlier
/// behavior for that token.
struct TokenContext {
    dialect: Dialect,
    brackets: Vec<Bracket>,
    /// The last code character since the last word, other than whitespace.
    last_punct: Option<char>,
    /// Rust: 1 after `#`, 2 after `#!`, when a `[` would open an attribute.
    hash: u8,
    annotation: Annotation,
    /// The last word, when nothing but whitespace follows it yet.
    previous_word: Option<Word>,
    /// The bracket depth of an open binding pattern: Rust `let`, `for` and closure parameters,
    /// JavaScript `const`, `let` and `var`, Python `for`, `lambda` and `as`.
    pattern: Option<usize>,
    /// The open pattern is a `let`, `const` or `var`, whose names are bound only once the
    /// statement's value is: in `let path = path.join(..);` the second `path` is not the local.
    pattern_defers: bool,
    /// Bindings not yet in effect, by index into `bindings`, with the bracket depth of their
    /// statement.
    deferred: Vec<(usize, usize)>,
    /// The 1-based line being read.
    line_number: u32,
    /// Where a deferred binding's own name stands.
    binding_sites: Vec<(u32, u32)>,
    /// Python: a `def` whose parameter list has not opened yet, then that list's depth.
    def_pending: bool,
    def_params: Option<usize>,
    /// Python: where a plain assignment's `=` (or an annotation's `:`) is on this line, when
    /// every word before it is a target.
    assignment_end: Option<usize>,
    bindings: Vec<(String, u32, u32)>,
}

impl TokenContext {
    fn new(language: &Language) -> Self {
        let dialect = match language {
            Language::Rust => Dialect::Rust,
            Language::TypeScript | Language::JavaScript => Dialect::Script,
            Language::Python => Dialect::Python,
            Language::Java => Dialect::Java,
            Language::Go => Dialect::Go,
            _ => Dialect::Other,
        };
        Self {
            dialect,
            brackets: Vec::new(),
            last_punct: None,
            hash: 0,
            annotation: Annotation::None,
            previous_word: None,
            pattern: None,
            pattern_defers: false,
            deferred: Vec::new(),
            line_number: 0,
            binding_sites: Vec::new(),
            def_pending: false,
            def_params: None,
            assignment_end: None,
            bindings: Vec::new(),
        }
    }

    fn start_line(&mut self, line: &str) {
        self.line_number += 1;
        // A Python or JavaScript statement may end with its line; a Rust one ends at its `;`.
        if self.dialect != Dialect::Rust {
            self.bind_deferred(0);
        }
        self.hash = 0;
        self.annotation = Annotation::None;
        self.assignment_end = None;
        if self.dialect == Dialect::Go {
            self.pattern = None;
            self.assignment_end = go_short_declaration_end(line);
        }
        if self.dialect == Dialect::Python {
            // A Python statement ends with its line unless a bracket is open.
            if self.brackets.is_empty() {
                self.pattern = None;
                self.assignment_end = python_assignment_end(line);
            }
            if self.def_params.is_none() {
                self.def_pending = false;
            }
        }
    }

    /// A literal or comment between two code spans of a line: an operand, as far as the
    /// punctuation around the next word goes.
    fn literal(&mut self) {
        self.last_punct = Some('"');
        self.previous_word = None;
        self.hash = 0;
        self.annotation = Annotation::None;
    }

    fn punct(&mut self, line: &str, idx: usize, ch: char) {
        let rest = &line[idx..];
        match ch {
            '#' if self.dialect == Dialect::Rust => {
                self.hash = 1;
                return;
            }
            '!' if self.hash == 1 => {
                self.hash = 2;
                return;
            }
            '@' if self.dialect != Dialect::Rust => {
                // A Python `@` elsewhere multiplies matrices; a decorator starts its line.
                if self.dialect != Dialect::Python || line[..idx].trim().is_empty() {
                    self.annotation = Annotation::Name;
                }
                return;
            }
            '.' if self.annotation == Annotation::Dot => self.annotation = Annotation::Name,
            _ => self.annotation = Annotation::None,
        }
        let opens_attribute = ch == '[' && self.hash > 0;
        self.hash = 0;
        if ch.is_whitespace() {
            return;
        }
        let previous_word = self.previous_word.take();
        match ch {
            '(' | '[' | '{' => {
                let bracket = match ch {
                    '(' => Bracket::Paren,
                    '[' if opens_attribute => Bracket::Attribute,
                    '[' => Bracket::Square,
                    _ => Bracket::Brace {
                        fields: self.dialect == Dialect::Rust && rust_field_brace(&line[..idx]),
                    },
                };
                if bracket == (Bracket::Brace { fields: false })
                    && self.dialect == Dialect::Rust
                    && self.pattern == Some(self.brackets.len())
                {
                    // `impl Trait for Type {`: the body is no pattern.
                    self.pattern = None;
                }
                if ch == '{' {
                    // `if let Some(x) = x {`: the block sees the local.
                    self.bind_deferred(idx as u32 + 1);
                }
                self.brackets.push(bracket);
                if ch == '(' && self.def_pending {
                    self.def_pending = false;
                    self.def_params = Some(self.brackets.len());
                }
            }
            ')' | ']' | '}' => {
                self.brackets.pop();
                let depth = self.brackets.len();
                if self.pattern.is_some_and(|start| depth < start) {
                    self.pattern = None;
                }
                if self.def_params.is_some_and(|params| depth < params) {
                    self.def_params = None;
                }
            }
            '=' => {
                let before = line[..idx].chars().next_back();
                let comparison = rest.starts_with("==")
                    || rest.starts_with("=>")
                    || before.is_some_and(|before| "=!<>".contains(before));
                if !comparison && self.pattern == Some(self.brackets.len()) {
                    self.pattern = None;
                }
            }
            ';' => {
                self.pattern = None;
                self.bind_deferred(idx as u32 + 1);
            }
            '|' if self.dialect == Dialect::Rust => {
                let depth = self.brackets.len();
                if self.pattern == Some(depth) {
                    // The `|` that closes a closure's parameters (or `||`, which has none).
                    self.pattern = None;
                } else if self.last_punct.is_some_and(|ch| "(,={[:&".contains(ch))
                    || previous_word == Some(Word::Move)
                {
                    // A `|` where an operand goes opens a closure's parameters; after an operand
                    // it is an or.
                    self.pattern = Some(depth);
                    self.pattern_defers = false;
                }
            }
            ':' if self.dialect == Dialect::Python && self.pattern == Some(self.brackets.len()) => {
                // The end of a `lambda`'s parameters.
                self.pattern = None;
            }
            _ => {}
        }
        self.last_punct = Some(ch);
    }

    /// Reads a word (keywords included) and returns the role of the token it spells.
    fn word(&mut self, line: &str, token: Range<usize>, line_index: usize) -> TokenRole {
        let text = &line[token.clone()];
        let before = line[..token.start].trim_end();
        let after = line[token.end..].trim_start();
        // `0.25` is a number, not a member `25`.
        let member = before.ends_with('.')
            && !before.ends_with("..")
            && !text.starts_with(|ch: char| ch.is_ascii_digit());
        let in_attribute =
            self.brackets.contains(&Bracket::Attribute) || self.annotation == Annotation::Name;
        self.annotation = if self.annotation == Annotation::Name {
            Annotation::Dot
        } else {
            Annotation::None
        };
        self.hash = 0;
        let innermost = self.brackets.last().copied();
        let starts_entry =
            |opening: &[char]| self.last_punct.is_some_and(|ch| opening.contains(&ch));
        let colon_follows = after.starts_with(':') && !after.starts_with("::");
        let field = match self.dialect {
            Dialect::Rust => {
                colon_follows
                    || (innermost == Some(Bracket::Brace { fields: true })
                        && starts_entry(&['{', ','])
                        && (after.is_empty() || after.starts_with([',', '}'])))
            }
            Dialect::Script => {
                (colon_follows || after.starts_with("?:"))
                    && matches!(innermost, Some(Bracket::Brace { .. } | Bracket::Paren))
                    && starts_entry(&['{', ',', '('])
            }
            Dialect::Python => {
                innermost == Some(Bracket::Paren)
                    && starts_entry(&['(', ','])
                    && after.starts_with('=')
                    && !after.starts_with("==")
            }
            // A composite literal's `Key: value`; `case X:` follows a word, not an entry.
            Dialect::Go => {
                colon_follows
                    && !after.starts_with(":=")
                    && matches!(innermost, Some(Bracket::Brace { .. }))
                    && starts_entry(&['{', ','])
            }
            Dialect::Java | Dialect::Other => false,
        };
        if !member && self.binds(text, before, after, colon_follows, token.end) {
            let depth = self.brackets.len();
            let defer = match self.dialect {
                Dialect::Rust | Dialect::Script => {
                    self.pattern_defers && self.pattern.is_some_and(|start| depth >= start)
                }
                Dialect::Python => {
                    self.assignment_end.is_some_and(|at| token.end <= at) && depth == 0
                }
                Dialect::Go => self.assignment_end.is_some_and(|at| token.end <= at),
                Dialect::Java | Dialect::Other => false,
            };
            if defer {
                let statement_depth = self.pattern.unwrap_or(depth);
                self.deferred.push((self.bindings.len(), statement_depth));
                self.bindings.push((text.to_string(), u32::MAX, u32::MAX));
                // The name it declares is no use of anything else.
                self.binding_sites
                    .push((line_index as u32 + 1, token.start as u32 + 1));
            } else {
                self.bindings.push((
                    text.to_string(),
                    line_index as u32 + 1,
                    token.start as u32 + 1,
                ));
            }
        }
        self.after_word(text);
        if in_attribute {
            TokenRole::Attribute
        } else if member {
            TokenRole::Member
        } else if field {
            TokenRole::Field
        } else {
            TokenRole::Name
        }
    }

    /// Whether the word at hand is a local binding the chunk shows: a pattern of a binding
    /// statement, a parameter, a Python assignment target or a Java declaration.
    fn binds(
        &self,
        text: &str,
        before: &str,
        after: &str,
        colon_follows: bool,
        end: usize,
    ) -> bool {
        let lowercase = text.starts_with(|ch: char| ch.is_lowercase() || ch == '_');
        let depth = self.brackets.len();
        let in_pattern = self.pattern.is_some_and(|start| depth >= start);
        match self.dialect {
            Dialect::Rust => {
                let names_item = after.starts_with(['(', '!', '{']) || after.starts_with("::");
                let param = colon_follows
                    && self.brackets.last() == Some(&Bracket::Paren)
                    && (self.last_punct.is_some_and(|ch| ch == '(' || ch == ',')
                        || self.previous_word == Some(Word::Mut));
                lowercase
                    && !before.ends_with("::")
                    && ((in_pattern && !names_item && self.last_punct != Some(':')) || param)
            }
            Dialect::Script => {
                let annotation = self.pattern == Some(depth) && self.last_punct == Some(':');
                in_pattern && !colon_follows && !annotation && !after.starts_with('(')
            }
            Dialect::Python => {
                let param = self.def_params == Some(depth)
                    && self.last_punct.is_none_or(|ch| "(,*".contains(ch));
                let target = self.assignment_end.is_some_and(|at| end <= at) && depth == 0;
                (in_pattern && !after.starts_with(['(', '.'])) || param || target
            }
            Dialect::Java => {
                let typed = matches!(self.previous_word, Some(Word::Type | Word::Var))
                    || (self.last_punct.is_some_and(|ch| ch == '>' || ch == ']')
                        && after.starts_with(['=', ';']));
                let ends = after.starts_with([';', ',', ')', ':'])
                    || (after.starts_with('=') && !after.starts_with("=="));
                lowercase && typed && ends
            }
            Dialect::Go => {
                let declared = self.assignment_end.is_some_and(|at| end <= at);
                let var = self.pattern.is_some_and(|start| depth >= start)
                    && self.previous_word == Some(Word::Var);
                declared || var
            }
            Dialect::Other => false,
        }
    }

    fn after_word(&mut self, text: &str) {
        let depth = self.brackets.len();
        match (self.dialect, text) {
            (Dialect::Rust, "let" | "for")
            | (Dialect::Script, "const" | "let" | "var")
            | (Dialect::Python, "for" | "lambda" | "as")
            | (Dialect::Go, "var") => {
                self.pattern = Some(depth);
                self.pattern_defers = matches!(text, "let" | "const" | "var");
            }
            (Dialect::Rust | Dialect::Python, "in") | (Dialect::Script, "of" | "in")
                if self.pattern == Some(depth) =>
            {
                self.pattern = None
            }
            (Dialect::Python, "def") => self.def_pending = true,
            (Dialect::Python, _)
                if self.pattern == Some(depth) && self.previous_word == Some(Word::As) =>
            {
                // `as` binds one name.
                self.pattern = None;
            }
            _ => {}
        }
        self.previous_word = Some(Word::of(text));
        self.last_punct = None;
    }

    /// Puts the deferred bindings whose statement ends here in effect from `column` of this
    /// line (0 at the start of a line).
    fn bind_deferred(&mut self, column: u32) {
        let depth = self.brackets.len();
        let line = self.line_number;
        let bindings = &mut self.bindings;
        self.deferred.retain(|(index, statement_depth)| {
            if *statement_depth < depth {
                return true;
            }
            if let Some(binding) = bindings.get_mut(*index) {
                binding.1 = line;
                binding.2 = column;
            }
            false
        });
    }

    /// Marks each plain name the chunk bound as a local at or before its use. A Java call is
    /// never of a local, so it keeps its role.
    fn mark_locals(&self, uses: &mut [TokenUse]) {
        if self.bindings.is_empty() {
            return;
        }
        // Where each name is first bound: a use at or after it is of the local.
        let mut first_bound = HashMap::<&str, (u32, u32)>::new();
        for (name, line, column) in &self.bindings {
            first_bound
                .entry(name.as_str())
                .and_modify(|at| *at = (*at).min((*line, *column)))
                .or_insert((*line, *column));
        }
        for token_use in uses.iter_mut() {
            if token_use.role != TokenRole::Name
                || !token_use.bare
                || (self.dialect == Dialect::Java && token_use.is_call)
            {
                continue;
            }
            let bound = first_bound
                .get(token_use.token.as_str())
                .is_some_and(|at| *at <= (token_use.line, token_use.column))
                || self
                    .binding_sites
                    .contains(&(token_use.line, token_use.column));
            if bound {
                token_use.role = TokenRole::Local;
            }
        }
    }
}

/// Whether a Rust `{` after `before` (its line up to the brace) opens a struct literal, struct
/// pattern or struct or enum body: a type path (`Foo`, `Self`, `a::Foo`) in a place an
/// expression, pattern or item name goes. `impl Foo {`, `-> Foo {`, `where T: Foo {` and
/// `x == MAX {` open blocks.
fn rust_field_brace(before: &str) -> bool {
    let before = before.trim_end();
    let path_start =
        trailing_run_start(before, |ch| ch.is_alphanumeric() || ch == '_' || ch == ':');
    let path = &before[path_start..];
    let type_named = path
        .rsplit("::")
        .next()
        .is_some_and(|last| last.starts_with(char::is_uppercase));
    if !type_named {
        return false;
    }
    let lead = before[..path_start].trim_end();
    // Only the words since the last brace or `;` qualify this one: `fn new() -> Self { Self {`.
    let statement = lead
        .rfind(['{', '}', ';'])
        .map_or(lead, |idx| &lead[idx + 1..]);
    let words = statement
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .collect::<Vec<_>>();
    if words
        .iter()
        .any(|word| matches!(*word, "fn" | "impl" | "trait" | "mod" | "where" | "for"))
    {
        return false;
    }
    let Some(last) = lead.chars().next_back() else {
        return true;
    };
    if last.is_alphanumeric() || last == '_' {
        let word = words.last().copied().unwrap_or_default();
        return matches!(
            word,
            "let" | "return" | "enum" | "struct" | "union" | "mut" | "yield" | "break"
        );
    }
    match last {
        '(' | ',' | '{' | '[' | '|' | ':' => true,
        '>' => lead.ends_with("=>"),
        '=' => !lead[..lead.len() - 1].ends_with(['=', '!', '<', '>']),
        '&' => !lead.ends_with("&&"),
        _ => false,
    }
}

/// Where a Go line's short variable declaration (`x := ...`, `a, b := ...`, also after `if`, `for`
/// or `switch`) puts its `:=`, when every word before it is a target name.
fn go_short_declaration_end(line: &str) -> Option<usize> {
    let end = line.find(":=")?;
    let targets = line[..end].trim();
    let targets = ["} else if ", "for ", "if ", "switch "]
        .iter()
        .find_map(|keyword| targets.strip_prefix(keyword))
        .unwrap_or(targets);
    let names = targets.split(',').all(|target| {
        let target = target.trim();
        !target.is_empty()
            && target.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            && !target.starts_with(|ch: char| ch.is_ascii_digit())
    });
    names.then_some(end)
}

/// Where a Python line's plain assignment (`a = ...`, `a, b = ...`, `a: int = ...`) puts its `=`
/// or annotation `:`, when every word before it is a target name.
fn python_assignment_end(line: &str) -> Option<usize> {
    let targets_end = line
        .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == ',' || ch.is_whitespace()))?;
    let targets = &line[..targets_end];
    let rest = &line[targets_end..];
    let assigns = (rest.starts_with('=') && !rest.starts_with("=="))
        || (rest.starts_with(':') && !rest[1..].trim().is_empty());
    let names = targets.split(',').all(|target| {
        let target = target.trim();
        !target.is_empty()
            && !target.contains(char::is_whitespace)
            && !target.starts_with(|ch: char| ch.is_ascii_digit())
    });
    (assigns && names && !is_keyword_or_literal(targets.trim())).then_some(targets_end)
}

/// Where a chunk's code is, as opposed to its comments and string literals, whose words name
/// nothing: `// the clock's now` and `"now"` are not uses of `now` (#563).
///
/// A lexical pass, not a parse. It knows each language's comment and literal delimiters, carries
/// block comments, multi-line literals and template interpolations across lines, and skips
/// JavaScript and TypeScript regular-expression literals. A chunk is read from its first line as
/// code: one that starts inside a block comment reads the comment's words as code up to its end,
/// and one that starts inside a multi-line string or template reads that text as code, after
/// which the closing quote opens a literal and the code that follows reads as literal up to the
/// next quote. A language it has no rules for (JSON, Markdown, plain text) is read whole, as
/// before; YAML and TOML lose only their `#` comments. A Python f-string's replacement fields are
/// code up to their conversion or format spec (#582); a Rust format string's names read as
/// literal.
pub(crate) struct CodeLexer {
    syntax: LexicalSyntax,
    /// Innermost last. Empty is code outside any template interpolation.
    stack: Vec<LexState>,
}

#[derive(Clone, Copy)]
struct LexicalSyntax {
    line_comment: Option<&'static str>,
    block_comments: bool,
    nested_block_comments: bool,
    double_quote_strings: bool,
    /// `'` opens a literal; with `lifetimes`, only when a character literal closes it.
    single_quote_strings: bool,
    lifetimes: bool,
    raw_strings: bool,
    triple_quotes: bool,
    backtick: Backtick,
    /// A `"` or `'` literal may continue on the next line.
    multiline_strings: bool,
    /// JavaScript: a `/` where an operand is expected opens a regular-expression literal.
    regex_literals: bool,
    /// Python: a literal with an `f` prefix holds `{...}` replacement fields, which are code.
    format_strings: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Backtick {
    Code,
    /// A JavaScript template literal, whose `${...}` interpolations are code.
    Template,
    /// A Go raw string.
    Raw,
}

#[derive(Clone, Copy, PartialEq)]
enum LexState {
    /// Code inside a template interpolation, with the depth of braces opened in it.
    Interpolation(u32),
    BlockComment(u32),
    Str {
        quote: u8,
        triple: bool,
    },
    RawStr {
        hashes: usize,
    },
    GoRaw,
    Template,
    /// A Python f-string; `raw` when an `r` prefix keeps its backslashes.
    FormatStr {
        quote: u8,
        triple: bool,
        raw: bool,
    },
    /// Code inside an f-string replacement field, with the depth of brackets opened in it: at
    /// depth 0 a `:` starts the format spec and a `}` closes the field.
    FormatField(u32),
    /// The format spec after a field's `:`, text except for nested `{...}` fields. Its `}` closes
    /// the field it belongs to.
    FormatSpec,
}

enum LexStep {
    Advance(usize),
    /// A literal that closes on this line, such as a regular expression: not code, no state.
    Skip(usize),
    Push(LexState, usize),
    Pop(usize),
    LineComment,
}

impl CodeLexer {
    pub(crate) fn new(language: &Language) -> Self {
        let c_like = LexicalSyntax {
            line_comment: Some("//"),
            block_comments: true,
            nested_block_comments: false,
            double_quote_strings: true,
            single_quote_strings: true,
            lifetimes: false,
            raw_strings: false,
            triple_quotes: false,
            backtick: Backtick::Code,
            multiline_strings: false,
            regex_literals: false,
            format_strings: false,
        };
        let plain = LexicalSyntax {
            line_comment: None,
            block_comments: false,
            double_quote_strings: false,
            single_quote_strings: false,
            ..c_like
        };
        let syntax = match language {
            Language::Rust => LexicalSyntax {
                nested_block_comments: true,
                lifetimes: true,
                raw_strings: true,
                multiline_strings: true,
                ..c_like
            },
            Language::TypeScript | Language::JavaScript => LexicalSyntax {
                backtick: Backtick::Template,
                regex_literals: true,
                ..c_like
            },
            Language::Java => LexicalSyntax {
                triple_quotes: true,
                ..c_like
            },
            Language::Go => LexicalSyntax {
                backtick: Backtick::Raw,
                ..c_like
            },
            Language::Python => LexicalSyntax {
                line_comment: Some("#"),
                block_comments: false,
                triple_quotes: true,
                format_strings: true,
                ..c_like
            },
            // A double-quoted SQL name is an identifier, not a literal.
            Language::Sql => LexicalSyntax {
                line_comment: Some("--"),
                double_quote_strings: false,
                multiline_strings: true,
                ..c_like
            },
            Language::Yaml | Language::Toml => LexicalSyntax {
                line_comment: Some("#"),
                ..plain
            },
            _ => plain,
        };
        Self {
            syntax,
            stack: Vec::new(),
        }
    }

    /// The byte ranges of `line` that are code, continuing from the previous line's state. Every
    /// range starts and ends beside an ASCII delimiter, so it is a valid slice of `line`.
    pub(crate) fn code_spans(&mut self, line: &str) -> Vec<Range<usize>> {
        let mut spans = Vec::new();
        let mut code_start = self.in_code().then_some(0);
        let mut idx = 0;
        while idx < line.len() {
            let step = match self.stack.last().copied() {
                None | Some(LexState::Interpolation(_) | LexState::FormatField(_)) => {
                    self.code_step(line, idx)
                }
                Some(state) => self.literal_step(state, &line.as_bytes()[idx..]),
            };
            match step {
                LexStep::Advance(len) => idx += len,
                LexStep::Skip(len) => {
                    close_span(&mut spans, &mut code_start, idx);
                    idx += len;
                    code_start = Some(idx);
                }
                LexStep::Push(state, len) => {
                    close_span(&mut spans, &mut code_start, idx);
                    idx += len;
                    self.stack.push(state);
                    if self.in_code() {
                        code_start = Some(idx);
                    }
                }
                LexStep::Pop(len) => {
                    close_span(&mut spans, &mut code_start, idx);
                    idx += len;
                    self.stack.pop();
                    if self.in_code() {
                        code_start = Some(idx);
                    }
                }
                LexStep::LineComment => {
                    close_span(&mut spans, &mut code_start, idx);
                    idx = line.len();
                }
            }
        }
        close_span(&mut spans, &mut code_start, line.len());
        // A literal that cannot span lines ends with its line, closed or not, so one stray quote
        // does not hide the rest of the chunk. An f-string's open fields end with it.
        if !self.syntax.multiline_strings {
            if let Some(open) = self.stack.iter().position(|state| {
                matches!(
                    state,
                    LexState::Str { triple: false, .. } | LexState::FormatStr { triple: false, .. }
                )
            }) {
                self.stack.truncate(open);
            }
        }
        spans
    }

    fn code_step(&mut self, line: &str, idx: usize) -> LexStep {
        let syntax = self.syntax;
        let rest = &line.as_bytes()[idx..];
        if syntax
            .line_comment
            .is_some_and(|marker| rest.starts_with(marker.as_bytes()))
        {
            return LexStep::LineComment;
        }
        if syntax.block_comments && rest.starts_with(b"/*") {
            return LexStep::Push(LexState::BlockComment(1), 2);
        }
        match rest[0] {
            b'r' if syntax.raw_strings => raw_string_open(line.as_bytes(), idx)
                .map_or(LexStep::Advance(1), |(hashes, len)| {
                    LexStep::Push(LexState::RawStr { hashes }, len)
                }),
            quote @ (b'"' | b'\'') => {
                let opens = if quote == b'"' {
                    syntax.double_quote_strings
                } else {
                    syntax.single_quote_strings
                };
                if !opens {
                    return LexStep::Advance(1);
                }
                if quote == b'\'' && syntax.lifetimes && !opens_char_literal(&line[idx + 1..]) {
                    return LexStep::Advance(1);
                }
                let triple = syntax.triple_quotes && rest.starts_with(&[quote; 3]);
                let len = if triple { 3 } else { 1 };
                match string_prefix(&line[..idx]).filter(|_| syntax.format_strings) {
                    Some(prefix) if prefix.contains(['f', 'F']) => LexStep::Push(
                        LexState::FormatStr {
                            quote,
                            triple,
                            raw: prefix.contains(['r', 'R']),
                        },
                        len,
                    ),
                    _ => LexStep::Push(LexState::Str { quote, triple }, len),
                }
            }
            b'/' if syntax.regex_literals && regex_may_start(&line[..idx]) => {
                regex_literal_len(rest).map_or(LexStep::Advance(1), LexStep::Skip)
            }
            b'`' => match syntax.backtick {
                Backtick::Template => LexStep::Push(LexState::Template, 1),
                Backtick::Raw => LexStep::Push(LexState::GoRaw, 1),
                Backtick::Code => LexStep::Advance(1),
            },
            bracket @ (b'(' | b'[' | b'{' | b')' | b']' | b'}' | b':')
                if matches!(self.stack.last(), Some(LexState::FormatField(_))) =>
            {
                self.format_field_step(bracket)
            }
            brace @ (b'{' | b'}') => match self.stack.last_mut() {
                Some(LexState::Interpolation(0)) if brace == b'}' => LexStep::Pop(1),
                Some(LexState::Interpolation(depth)) => {
                    if brace == b'{' {
                        *depth += 1;
                    } else {
                        *depth -= 1;
                    }
                    LexStep::Advance(1)
                }
                _ => LexStep::Advance(1),
            },
            _ => LexStep::Advance(1),
        }
    }

    fn literal_step(&mut self, state: LexState, rest: &[u8]) -> LexStep {
        match state {
            LexState::BlockComment(depth) => {
                if rest.starts_with(b"*/") {
                    if depth == 1 {
                        return LexStep::Pop(2);
                    }
                    self.replace_top(LexState::BlockComment(depth - 1));
                    LexStep::Advance(2)
                } else if self.syntax.nested_block_comments && rest.starts_with(b"/*") {
                    self.replace_top(LexState::BlockComment(depth + 1));
                    LexStep::Advance(2)
                } else {
                    LexStep::Advance(1)
                }
            }
            LexState::Str { quote, triple } => {
                if rest[0] == b'\\' {
                    LexStep::Advance(2.min(rest.len()))
                } else if triple && rest.starts_with(&[quote; 3]) {
                    LexStep::Pop(3)
                } else if !triple && rest[0] == quote {
                    LexStep::Pop(1)
                } else {
                    LexStep::Advance(1)
                }
            }
            LexState::RawStr { hashes } => {
                let closes = rest[0] == b'"'
                    && rest.len() > hashes
                    && rest[1..=hashes].iter().all(|byte| *byte == b'#');
                if closes {
                    LexStep::Pop(1 + hashes)
                } else {
                    LexStep::Advance(1)
                }
            }
            LexState::GoRaw if rest[0] == b'`' => LexStep::Pop(1),
            LexState::GoRaw => LexStep::Advance(1),
            LexState::Template => {
                if rest[0] == b'\\' {
                    LexStep::Advance(2.min(rest.len()))
                } else if rest[0] == b'`' {
                    LexStep::Pop(1)
                } else if rest.starts_with(b"${") {
                    LexStep::Push(LexState::Interpolation(0), 2)
                } else {
                    LexStep::Advance(1)
                }
            }
            LexState::FormatStr { quote, triple, raw } => {
                if rest.starts_with(b"\\N{") && !raw {
                    // A named escape, `\N{BULLET}`, is text.
                    let close = rest.iter().position(|byte| *byte == b'}');
                    LexStep::Advance(close.map_or(rest.len(), |idx| idx + 1))
                } else if rest[0] == b'\\' && !raw {
                    LexStep::Advance(2.min(rest.len()))
                } else if triple && rest.starts_with(&[quote; 3]) {
                    LexStep::Pop(3)
                } else if !triple && rest[0] == quote {
                    LexStep::Pop(1)
                } else if rest.starts_with(b"{{") || rest.starts_with(b"}}") {
                    LexStep::Advance(2)
                } else if rest[0] == b'{' {
                    LexStep::Push(LexState::FormatField(0), 1)
                } else {
                    LexStep::Advance(1)
                }
            }
            LexState::FormatSpec => match rest[0] {
                b'{' => LexStep::Push(LexState::FormatField(0), 1),
                b'}' => LexStep::Pop(1),
                _ => LexStep::Advance(1),
            },
            // Code states are stepped by `code_step`.
            LexState::Interpolation(_) | LexState::FormatField(_) => LexStep::Advance(1),
        }
    }

    /// A bracket or `:` inside an f-string replacement field. Only one at the field's own depth
    /// ends its code: `}` closes the field, and `:` starts the format spec, whose `}` closes it.
    /// A `!r` conversion reads as code, and its one letter names nothing.
    fn format_field_step(&mut self, byte: u8) -> LexStep {
        let Some(LexState::FormatField(depth)) = self.stack.last().copied() else {
            return LexStep::Advance(1);
        };
        match (byte, depth) {
            (b'}', 0) => LexStep::Pop(1),
            (b':', 0) => {
                // The spec is text, so the field's code ends at the `:`.
                self.stack.pop();
                LexStep::Push(LexState::FormatSpec, 1)
            }
            (b':', _) => LexStep::Advance(1),
            (b'(' | b'[' | b'{', _) => {
                self.replace_top(LexState::FormatField(depth + 1));
                LexStep::Advance(1)
            }
            _ => {
                self.replace_top(LexState::FormatField(depth.saturating_sub(1)));
                LexStep::Advance(1)
            }
        }
    }

    pub(crate) fn in_code(&self) -> bool {
        matches!(
            self.stack.last(),
            None | Some(LexState::Interpolation(_) | LexState::FormatField(_))
        )
    }

    fn replace_top(&mut self, state: LexState) {
        if let Some(top) = self.stack.last_mut() {
            *top = state;
        }
    }
}

/// Whether a `/` after `before` (its line up to the `/`) starts a regular expression rather than
/// dividing: where an operand is expected, after an operator, an opening bracket, the start of
/// the line or a keyword that takes an expression.
fn regex_may_start(before: &str) -> bool {
    let before = before.trim_end();
    let Some(last) = before.chars().next_back() else {
        return true;
    };
    // Not `<`: in JSX `</Tag>` closes an element, and a regex after `<` is rare enough to lose.
    if "(,=:[!&|?{};+-*%>~^".contains(last) {
        return true;
    }
    let word = before
        .rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
        .next()
        .unwrap_or_default();
    matches!(
        word,
        "return"
            | "typeof"
            | "case"
            | "do"
            | "else"
            | "in"
            | "of"
            | "yield"
            | "await"
            | "void"
            | "delete"
            | "throw"
    )
}

/// The length of the regular-expression literal opening at `rest[0]`, flags included, when its
/// closing `/` (unescaped, outside a `[...]` class) is on this line; otherwise the `/` divides.
fn regex_literal_len(rest: &[u8]) -> Option<usize> {
    let mut idx = 1;
    let mut in_class = false;
    while idx < rest.len() {
        match rest[idx] {
            b'\\' => idx += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                let flags = rest[idx + 1..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_alphabetic())
                    .count();
                return Some(idx + 1 + flags);
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn close_span(spans: &mut Vec<Range<usize>>, code_start: &mut Option<usize>, end: usize) {
    if let Some(start) = code_start.take() {
        if start < end {
            spans.push(start..end);
        }
    }
}

/// `r"`, `r#"`, `br##"` and the like at `idx`, the `r`: the number of `#`s and the length of the
/// opening delimiter from `r` on. `r#type` is a raw identifier, not a string.
fn raw_string_open(bytes: &[u8], idx: usize) -> Option<(usize, usize)> {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii();
    let prefix_ok = match idx.checked_sub(1).map(|before| bytes[before]) {
        None => true,
        Some(b'b' | b'c') => idx < 2 || !is_word(bytes[idx - 2]),
        Some(byte) => !is_word(byte),
    };
    if !prefix_ok {
        return None;
    }
    let hashes = bytes[idx + 1..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    (bytes.get(idx + 1 + hashes) == Some(&b'"')).then_some((hashes, hashes + 2))
}

/// The string prefix (`f`, `rb`, `Rf`...) that ends `before`, the line up to a quote: its last
/// word, when that is at most two prefix letters.
fn string_prefix(before: &str) -> Option<&str> {
    let start = trailing_run_start(before, |ch| ch.is_alphanumeric() || ch == '_');
    let word = &before[start..];
    (!word.is_empty() && word.len() <= 2 && word.chars().all(|ch| "bBrRfFuU".contains(ch)))
        .then_some(word)
}

/// After a Rust `'`: whether a character literal follows (one character or an escape, then `'`)
/// rather than a lifetime or label.
fn opens_char_literal(after_quote: &str) -> bool {
    let mut chars = after_quote.chars();
    match chars.next() {
        Some('\\') => true,
        Some(_) => chars.next() == Some('\''),
        None => false,
    }
}

/// TypeScript and JavaScript import each other and share one module system, so a name defined
/// in one is used from the other. No other pair of indexed languages reaches the other's names
/// without an explicit binding layer, so every other language is a family of its own.
fn language_family(language: &Language) -> &'static str {
    match language {
        Language::TypeScript | Language::JavaScript => "javascript",
        other => other.key(),
    }
}

/// The candidates of a name-only strategy, or none when not one is in the token's language
/// family: a Rust `Utc::now()` is not a call of the one JavaScript `now` in the repository (#563).
/// A candidate in another language still counts against the match when one in the family
/// exists: that the name is defined twice says it is common, whichever definition the token can
/// reach, and dropping the other would turn `x.find(..)` into a call of the one Rust `find`.
fn in_language_family(chunk: &CodeChunk, candidates: Vec<Symbol>) -> Vec<Symbol> {
    let family = language_family(&chunk.language);
    if candidates
        .iter()
        .any(|symbol| language_family(&symbol.language) == family)
    {
        candidates
    } else {
        Vec::new()
    }
}

fn push_token_use(
    uses: &mut Vec<TokenUse>,
    language: &Language,
    line: &str,
    range: Range<usize>,
    line_index: usize,
    role: TokenRole,
) {
    let token = &line[range.clone()];
    let token_end = range.end;
    if is_keyword_or_literal(token) || token.len() < 2 {
        return;
    }
    let qualifier = (*language == Language::Rust)
        .then(|| path_qualifier(&line[..range.start]))
        .flatten();
    let receiver = (role == TokenRole::Member)
        .then(|| member_receiver(line, range.start))
        .flatten();
    let is_call = line[token_end..]
        .chars()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == '(');
    let token_start = token_end - token.len();
    let before = line[..token_start].trim_end();
    let declares_module = before
        .strip_suffix("mod")
        .is_some_and(|rest| rest.is_empty() || rest.ends_with(char::is_whitespace));
    let bare = !(before.ends_with("::")
        || (before.ends_with('.') && !before.ends_with(".."))
        || declares_module);
    uses.push(TokenUse {
        token: token.to_string(),
        line: line_index as u32 + 1,
        column: token_start as u32 + 1,
        is_call,
        bare,
        role,
        qualifier,
        receiver,
    });
}

/// The plain name before the `.` of a member at `start`: a word not itself a member or the
/// result of a call or index, and not `self`, `this`, `super` or `cls`. `None` on an import or
/// package line, whose dotted path names modules rather than members.
fn member_receiver(line: &str, start: usize) -> Option<String> {
    let statement = line.trim_start();
    if ["import ", "from ", "package ", "use ", "pub use "]
        .iter()
        .any(|keyword| statement.starts_with(keyword))
    {
        return None;
    }
    let before = line[..start].trim_end();
    let before = before
        .strip_suffix("?.")
        .or_else(|| before.strip_suffix('.'))?
        .trim_end();
    let word_start =
        trailing_run_start(before, |ch| ch.is_alphanumeric() || ch == '_' || ch == '$');
    let word = &before[word_start..];
    let head = !before[..word_start].trim_end().ends_with(['.', '?']);
    (head
        && !word.is_empty()
        && !word.starts_with(|ch: char| ch.is_ascii_digit())
        && !matches!(word, "self" | "this" | "super" | "cls" | "Self"))
    .then(|| word.to_string())
}

/// Where the run of characters `keep` accepts that ends `text` starts, as a byte offset on a
/// character boundary: the character before the run may be any width (`·`, `—`).
fn trailing_run_start(text: &str, keep: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .rev()
        .find(|(_, ch)| !keep(*ch))
        .map_or(0, |(idx, ch)| idx + ch.len_utf8())
}

/// The path segment a Rust token is the tail of: `mem` before `take` in `std::mem::take(..)`.
/// `None` for a bare name, and for a path whose segment is not a plain name (`Vec::<u8>::new`,
/// `<T as Trait>::name`), which is matched as before.
fn path_qualifier(before: &str) -> Option<String> {
    let path = before.trim_end().strip_suffix("::")?.trim_end();
    let start = trailing_run_start(path, |ch| ch.is_alphanumeric() || ch == '_');
    let segment = &path[start..];
    (!segment.is_empty()).then(|| segment.to_string())
}

/// Whether a path segment spelled at a use site (`open_kioku_core`, `generations`, `SqliteStore`)
/// is part of where `symbol` is defined: a segment of its qualified name, where a crate's `-`
/// is spelled `_`, or the name of the item it belongs to.
fn segment_locates(registry: &SymbolRegistry, segment: &str, symbol: &Symbol) -> bool {
    let same = |candidate: &str| {
        candidate.len() == segment.len()
            && candidate
                .bytes()
                .zip(segment.bytes())
                .all(|(a, b)| a == b || (a == b'-' && b == b'_'))
    };
    let path = symbol
        .qualified_name
        .rsplit_once("::")
        .map_or("", |(path, _)| path);
    path.split("::").any(same)
        || symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|parent| registry.by_id.get(parent))
            .is_some_and(|parent| parent.name == segment)
}

/// The class a Java static import takes its member from, by its path: `org.example.Constants`
/// of `static org.example.Constants.ACCESS_KEY`. `None` for any other import.
fn static_import_owner(binding: &ImportBinding) -> Option<&str> {
    let path = binding.source_module.strip_prefix("static ")?.trim();
    let (owner, _member) = path.rsplit_once('.')?;
    (!owner.is_empty()).then_some(owner)
}

/// Whether `symbol` is a member of the Java class the dotted path `owner` names: the package its
/// file declares (`package`, empty for none) followed by the classes enclosing it, outermost first.
/// `org.example.Constants` names a member of the top-level `Constants` of a file declaring
/// `package org.example;`, and `org.example.Outer.Inner` one of the class `Inner` nested in it,
/// wherever the file sits: its directory need not mirror its package (#617).
fn owner_locates(registry: &SymbolRegistry, owner: &str, package: &str, symbol: &Symbol) -> bool {
    let mut classes = Vec::new();
    let mut parent = symbol.parent_symbol_id.as_ref();
    while let Some(class) = parent.and_then(|id| registry.by_id.get(id)) {
        // A symbol's parents never cycle, but the registry holds what the index stored.
        if classes.len() > MAX_ENCLOSING_CLASSES {
            return false;
        }
        classes.push(class.name.as_str());
        parent = class.parent_symbol_id.as_ref();
    }
    if classes.is_empty() {
        return false;
    }
    let declared = package.split('.').filter(|segment| !segment.is_empty());
    owner
        .split('.')
        .eq(declared.chain(classes.into_iter().rev()))
}

/// Whether the directory `path` is `dir` joined with `rest`, either of which may be empty.
fn joined_path_is(path: &str, dir: &str, rest: &str) -> bool {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => path == rest,
        (false, true) => path == dir,
        (false, false) => path
            .strip_prefix(dir)
            .and_then(|tail| tail.strip_prefix('/'))
            .is_some_and(|tail| tail == rest),
    }
}

fn symbol_matches_token(symbol: &Symbol, token: &str) -> bool {
    symbol.name == token
        || (symbol.qualified_name.len() > token.len() + 2
            && symbol.qualified_name.ends_with(token)
            && symbol.qualified_name.as_bytes()[symbol.qualified_name.len() - token.len() - 2..]
                .starts_with(b"::"))
}

fn import_mentions_token(import: &ImportResolution, token: &str) -> bool {
    import
        .import
        .imported
        .rsplit(['/', '.', ':'])
        .next()
        .is_some_and(|last| last == token)
}

fn module_name(qualified_name: &str) -> String {
    qualified_name
        .rsplit_once("::")
        .map(|(module, _)| module.to_string())
        .unwrap_or_default()
}

fn qualified_name_suffix(qualified_name: &str) -> String {
    qualified_name
        .rsplit_once("::")
        .or_else(|| qualified_name.rsplit_once('.'))
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| qualified_name.to_string())
}

fn graph_node_type(symbol: &Symbol) -> GraphNodeType {
    match symbol.kind {
        SymbolKind::Class => GraphNodeType::Class,
        SymbolKind::Trait => GraphNodeType::Trait,
        SymbolKind::Interface => GraphNodeType::Interface,
        SymbolKind::Method => GraphNodeType::Method,
        SymbolKind::Field => GraphNodeType::Field,
        SymbolKind::Endpoint => GraphNodeType::Endpoint,
        SymbolKind::DatabaseTable => GraphNodeType::DatabaseTable,
        SymbolKind::Test => GraphNodeType::Test,
        SymbolKind::Module | SymbolKind::Package => GraphNodeType::Module,
        _ => GraphNodeType::Function,
    }
}

fn symbol_rank(symbol: &Symbol) -> (u8, usize) {
    let kind_rank = match symbol.kind {
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface => 0,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Endpoint => 1,
        SymbolKind::Module | SymbolKind::Package => 2,
        SymbolKind::Variable | SymbolKind::Constant | SymbolKind::Field => 3,
        SymbolKind::DatabaseTable | SymbolKind::Test | SymbolKind::Unknown => 4,
    };
    (kind_rank, symbol.qualified_name.len())
}

fn is_keyword_or_literal(token: &str) -> bool {
    matches!(
        token,
        "if" | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "let"
            | "const"
            | "var"
            | "function"
            | "fn"
            | "class"
            | "struct"
            | "enum"
            | "trait"
            | "interface"
            | "impl"
            | "pub"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "new"
            | "true"
            | "false"
            | "null"
            | "None"
            | "Some"
            | "Ok"
            | "Err"
            | "self"
            | "this"
            | "super"
            | "crate"
            | "import"
            | "from"
            | "use"
            | "package"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Import, LineRange};

    fn symbol(id: &str, file: &str, name: &str, qualified: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified.into(),
            kind,
            file_id: FileId::new(file),
            range: Some(LineRange::single(1)),
            language: open_kioku_core::Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        }
    }

    fn chunk(id: &str, file: &str, symbol_id: Option<&str>, text: &str) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file_id: FileId::new(file),
            range: LineRange::single(1),
            language: open_kioku_core::Language::TypeScript,
            text: text.into(),
            symbol_id: symbol_id.map(SymbolId::new),
        }
    }

    fn import_resolution(file: &str, imported: &str, target_file: &str) -> ImportResolution {
        ImportResolution {
            import: Import {
                file_id: FileId::new(file),
                imported: imported.into(),
                range: Some(LineRange::single(1)),
                confidence: Confidence::Medium,
            },
            status: ResolutionStatus::Resolved,
            target_file: Some(FileId::new(target_file)),
            target_symbol: None,
            confidence: Confidence::High,
            strategy: "test-import".into(),
            caveats: vec![],
        }
    }

    #[test]
    fn direct_import_resolves_call() {
        let symbols = vec![
            symbol("caller", "entry", "main", "src::main", SymbolKind::Function),
            symbol(
                "target",
                "util",
                "helper",
                "src::util::helper",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "helper();")],
            &symbols,
            &[import_resolution("entry", "./util", "util")],
            false,
            None,
        );
        let fact = report
            .analysis_facts
            .iter()
            .find(|fact| fact.target == "src::util::helper")
            .unwrap();
        assert_eq!(fact.edge_type, GraphEdgeType::Calls);
        assert_eq!(fact.confidence, Confidence::High);
        assert!(fact.source.contains("direct-import"));
    }

    #[test]
    fn same_file_and_same_module_resolution() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "local",
                "entry",
                "local",
                "app::local",
                SymbolKind::Function,
            ),
            symbol(
                "neighbor",
                "other",
                "neighbor",
                "app::neighbor",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "local(); neighbor();")],
            &symbols,
            &[],
            false,
            None,
        );
        assert!(report
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "app::local" && fact.source.contains("same-file")));
        assert!(report
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "app::neighbor" && fact.source.contains("same-module")));
    }

    #[test]
    fn unique_project_name_is_medium_confidence() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "unique",
                "other",
                "unique",
                "lib::unique",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "unique();")],
            &symbols,
            &[],
            false,
            None,
        );
        let fact = report
            .analysis_facts
            .iter()
            .find(|fact| fact.target == "lib::unique")
            .unwrap();
        assert_eq!(fact.confidence, Confidence::Medium);
        assert!(fact.message.contains("speculative=true"));
    }

    #[test]
    fn suffix_ambiguity_and_common_name_caps_surface_caveats() {
        let mut symbols = vec![symbol(
            "caller",
            "entry",
            "main",
            "app::main",
            SymbolKind::Function,
        )];
        for index in 0..(COMMON_NAME_CAP + 1) {
            symbols.push(symbol(
                &format!("common-{index}"),
                &format!("file-{index}"),
                "render",
                &format!("pkg{index}::render"),
                SymbolKind::Function,
            ));
        }
        symbols.push(symbol(
            "amb-a",
            "a",
            "Session",
            "pkg::a::Session",
            SymbolKind::Class,
        ));
        symbols.push(symbol(
            "amb-b",
            "b",
            "Session",
            "pkg::b::Session",
            SymbolKind::Class,
        ));
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "render(); Session;")],
            &symbols,
            &[],
            false,
            None,
        );
        assert!(report.quality_notes.iter().any(|note| {
            note.kind == QualityNoteKind::SymbolRegistryCaveat
                && note.message.contains("common name `render`")
        }));
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.message.contains("2 candidates matched")));
    }

    #[test]
    fn unresolved_calls_surface_low_confidence_notes() {
        let symbols = vec![symbol(
            "caller",
            "entry",
            "main",
            "app::main",
            SymbolKind::Function,
        )];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "missingCall();")],
            &symbols,
            &[],
            false,
            None,
        );
        assert!(report.analysis_facts.is_empty());
        assert!(report.quality_notes.iter().any(|note| {
            note.kind == QualityNoteKind::SymbolRegistryUnresolved
                && note.message.contains("missingCall")
        }));
    }

    #[test]
    fn unresolved_note_cap_follows_chunk_order_not_worker_scheduling() {
        let symbols = vec![symbol(
            "caller",
            "entry",
            "main",
            "app::main",
            SymbolKind::Function,
        )];
        // Three distinct unresolved calls per chunk, one repeated on a later line: 30 chunks
        // overfill the cap, and the repeat must not spend a second slot.
        let chunks = (0..30)
            .map(|index| {
                chunk(
                    &format!("c{index:02}"),
                    "entry",
                    Some("caller"),
                    &format!(
                        "absentAlpha{index:02}();\nabsentBravo{index:02}();\nabsentCharlie{index:02}();\nabsentAlpha{index:02}();"
                    ),
                )
            })
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        for index in 0..=MAX_UNRESOLVED_NOTES / 3 {
            for name in ["Alpha", "Bravo", "Charlie"] {
                if expected.len() < MAX_UNRESOLVED_NOTES {
                    expected.push(format!(
                        "symbol registry unresolved `absent{name}{index:02}` in chunk c{index:02}"
                    ));
                }
            }
        }
        expected.sort();
        for threads in [1, 2, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for _ in 0..5 {
                let report =
                    pool.install(|| resolve_symbol_edges(&chunks, &symbols, &[], false, None));
                let unresolved = report
                    .quality_notes
                    .iter()
                    .filter(|note| note.kind == QualityNoteKind::SymbolRegistryUnresolved)
                    .map(|note| note.message.clone())
                    .filter(|message| message.contains("in chunk"))
                    .collect::<Vec<_>>();
                assert_eq!(unresolved, expected, "{threads} worker thread(s)");
                // 30 chunks * 3 distinct names = 90, so the cap hides 26 of them and says so.
                assert!(
                    report.quality_notes.iter().any(|note| note.message
                        == format!(
                            "symbol registry unresolved cap is {MAX_UNRESOLVED_NOTES}; {} more unresolved name(s) not listed (90 total)",
                            90 - MAX_UNRESOLVED_NOTES
                        )),
                    "the cap must report how many names it withheld: {:?}",
                    report.quality_notes
                );
            }
        }
    }

    #[test]
    fn repeated_candidates_are_counted_once_in_any_order() {
        let first = symbol("a", "x", "Session", "pkg::Session", SymbolKind::Class);
        let second = symbol("b", "y", "Session", "pkg::Session", SymbolKind::Class);
        for candidates in [
            vec![first.clone(), second.clone(), first.clone()],
            vec![second.clone(), first.clone(), first.clone()],
            vec![first.clone(), first.clone(), second.clone()],
        ] {
            let resolution =
                resolution_from_candidates("test", candidates, Confidence::High, false).unwrap();
            assert_eq!(resolution.candidates, 2);
            assert!(resolution.symbol.is_none());
        }
    }

    #[test]
    fn a_call_takes_the_lines_edge_over_a_reference_to_the_same_target_in_either_order() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "entry",
                "path",
                "app::Store::path",
                SymbolKind::Method,
            ),
        ];
        // The same bare and member uses in both orders: the edge a line gets must not depend
        // on which use comes first (#534).
        for text in ["let path = dir.path();", "dir.path(); path;"] {
            let report = resolve_symbol_edges(
                &[chunk("c1", "entry", Some("caller"), text)],
                &symbols,
                &[],
                false,
                None,
            );
            let edges = report
                .analysis_facts
                .iter()
                .filter(|fact| fact.target == "app::Store::path")
                .map(|fact| fact.edge_type.clone())
                .collect::<Vec<_>>();
            assert_eq!(edges, vec![GraphEdgeType::Calls], "{text}");
        }
    }

    fn with_language(mut symbol: Symbol, language: Language) -> Symbol {
        symbol.language = language;
        symbol
    }

    fn rust_symbol(id: &str, file: &str, name: &str, qualified: &str) -> Symbol {
        with_language(
            symbol(id, file, name, qualified, SymbolKind::Function),
            Language::Rust,
        )
    }

    fn chunk_in(language: Language, text: &str) -> CodeChunk {
        CodeChunk {
            language,
            ..chunk("c1", "entry", Some("caller"), text)
        }
    }

    /// Each fact's target, edge type and line, in line order.
    fn targets(report: &RegistryReport) -> Vec<(String, GraphEdgeType, u32)> {
        let mut targets = report
            .analysis_facts
            .iter()
            .map(|fact| {
                (
                    fact.target.clone(),
                    fact.edge_type.clone(),
                    fact.range.as_ref().map_or(0, |range| range.start),
                )
            })
            .collect::<Vec<_>>();
        targets.sort_by_key(|(_, _, line)| *line);
        targets
    }

    fn call(target: &str, line: u32) -> (String, GraphEdgeType, u32) {
        (target.to_string(), GraphEdgeType::Calls, line)
    }

    #[test]
    fn unique_project_name_matches_only_its_own_language_family() {
        // The one `now` in the repository is JavaScript: a Rust `now()` is not a call of it.
        let javascript_now = with_language(
            symbol("js-now", "site", "now", "site::now", SymbolKind::Function),
            Language::JavaScript,
        );
        let mut symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            javascript_now,
        ];
        let rust_call = chunk_in(Language::Rust, "let at = now();");
        let report =
            resolve_symbol_edges(std::slice::from_ref(&rust_call), &symbols, &[], false, None);
        assert_eq!(targets(&report), vec![], "{:?}", report.analysis_facts);

        // A TypeScript file reaches JavaScript's names, so the family match still links it.
        let typescript_call = chunk_in(Language::TypeScript, "const at = now();");
        let report = resolve_symbol_edges(&[typescript_call], &symbols, &[], false, None);
        assert_eq!(targets(&report), vec![call("site::now", 1)]);

        // Beside a Rust `now` the name is defined twice, so it is not unique: the JavaScript
        // definition cannot be the target, and it still says the name is common.
        symbols.push(rust_symbol("rs-now", "clock", "now", "clock::now"));
        let report = resolve_symbol_edges(&[rust_call], &symbols, &[], false, None);
        assert_eq!(targets(&report), vec![]);
        assert!(report.quality_notes.iter().any(|note| note
            .message
            .contains("2 candidates matched via unique-project-name")));
    }

    #[test]
    fn name_fallbacks_do_not_cross_languages_either() {
        // Same qualified-name module, suffix reachability and fuzzy matching are name-only too.
        let python_lines = with_language(
            symbol(
                "py",
                "tool",
                "read_lines",
                "app::read_lines",
                SymbolKind::Function,
            ),
            Language::Python,
        );
        let symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            python_lines,
        ];
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Rust, "read_lines(); lines();")],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![], "{:?}", report.analysis_facts);
    }

    #[test]
    fn tokens_in_comments_resolve_to_nothing() {
        let symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            rust_symbol("target", "util", "helper", "util::helper"),
        ];
        let text = "// helper() runs first\n/// see helper()\n/* outer /* helper() */ still helper() */\nlet x = 1; // helper()";
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Rust, text)],
            &symbols,
            &[import_resolution("entry", "crate::util::helper", "util")],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![], "{:?}", report.analysis_facts);
        // Nor does a name in prose surface as unresolved.
        assert!(!report
            .quality_notes
            .iter()
            .any(|note| note.message.contains("`runs`")));

        let python = chunk_in(Language::Python, "# helper() here\nvalue = 1  # helper()");
        let report = resolve_symbol_edges(&[python], &symbols, &[], false, None);
        assert_eq!(targets(&report), vec![]);
    }

    #[test]
    fn tokens_in_string_literals_resolve_to_nothing() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "util",
                "helper",
                "util::helper",
                SymbolKind::Function,
            ),
        ];
        for text in [
            r#"log("helper() failed");"#,
            "log('helper() failed');",
            "log(`helper() failed`);",
            "log(\"a \\\" helper()\");",
        ] {
            let report = resolve_symbol_edges(
                &[chunk_in(Language::TypeScript, text)],
                &symbols,
                &[],
                false,
                None,
            );
            assert_eq!(targets(&report), vec![], "{text}");
        }
        // A template interpolation is code, and so is what follows the literal.
        let report = resolve_symbol_edges(
            &[chunk_in(
                Language::TypeScript,
                "log(`helper ${helper({ a: 1 })} helper`);\nhelper;",
            )],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(
            targets(&report),
            vec![
                call("util::helper", 1),
                ("util::helper".to_string(), GraphEdgeType::References, 2),
            ]
        );
    }

    #[test]
    fn rust_and_python_literals_are_read_by_their_own_delimiters() {
        let symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            rust_symbol("target", "util", "helper", "util::helper"),
        ];
        // Lifetimes open no literal; raw strings, byte strings and multi-line strings close
        // where Rust closes them, and code after them is still read.
        let text = "fn f<'a>(x: &'a str) -> char {\nlet s = r#\"helper() \"quoted\" \"#;\nlet b = br\"helper()\";\nlet m = \"first\nhelper() second\";\nlet c = '\"'; let q = '\\'';\nhelper()\n}";
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Rust, text)],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![call("util::helper", 7)]);

        let python_symbols = symbols
            .iter()
            .cloned()
            .map(|symbol| with_language(symbol, Language::Python))
            .collect::<Vec<_>>();
        let text = "doc = \"\"\"\nhelper() is documented\n\"\"\"\nname = rb'helper()'\nhelper()";
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Python, text)],
            &python_symbols,
            &[],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![call("util::helper", 5)]);
        assert!(!report
            .quality_notes
            .iter()
            .any(|note| note.message.contains("`rb`")));
    }

    #[test]
    fn javascript_regex_literals_hide_no_code_after_them() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "util",
                "helper",
                "util::helper",
                SymbolKind::Function,
            ),
        ];
        // Each regex holds a delimiter that would open a template, comment or string; the code
        // after it on its line and on the next must still be read, and the regex body must not.
        for regex in [
            r"/`/",
            r"/\/*/",
            r#"/"/"#,
            r"/'/g",
            r"/a\/\/b/",
            r"/[/`]helper/i",
            "/x/",
        ] {
            let text =
                format!("const re = {regex}; helper();\nreturn {regex}.test(s) && helper();");
            let report = resolve_symbol_edges(
                &[chunk_in(Language::TypeScript, &text)],
                &symbols,
                &[],
                false,
                None,
            );
            assert_eq!(
                targets(&report),
                vec![call("util::helper", 1), call("util::helper", 2)],
                "{text}"
            );
        }
        // A JSX closing tag opens no regex: the names after it on the line are still code.
        let jsx_symbols = vec![
            symbol("color", "ui", "color", "ui::color", SymbolKind::Variable),
            symbol(
                "org",
                "ui",
                "organizationName",
                "ui::organizationName",
                SymbolKind::Variable,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk_in(
                Language::TypeScript,
                r#"<Text bold>Org:</Text> <Text color="white">{organizationName}</Text>"#,
            )],
            &jsx_symbols,
            &[],
            false,
            None,
        );
        let mut jsx_targets = targets(&report)
            .into_iter()
            .map(|(target, _, _)| target)
            .collect::<Vec<_>>();
        jsx_targets.sort();
        assert_eq!(jsx_targets, vec!["ui::color", "ui::organizationName"]);
        // A `/` after an operand divides, and a call between two of them is code.
        let report = resolve_symbol_edges(
            &[chunk_in(
                Language::JavaScript,
                "const r = total / helper() / 2;",
            )],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![call("util::helper", 1)]);
    }

    #[test]
    fn languages_without_literal_rules_are_read_whole() {
        for language in [Language::Json, Language::Markdown, Language::Text] {
            let symbols = vec![with_language(
                symbol(
                    "target",
                    "util",
                    "helper",
                    "util::helper",
                    SymbolKind::Function,
                ),
                language.clone(),
            )];
            let report = resolve_symbol_edges(
                &[chunk_in(language.clone(), r#"{"helper": "value"}"#)],
                &symbols,
                &[],
                false,
                None,
            );
            assert_eq!(
                targets(&report),
                vec![("util::helper".to_string(), GraphEdgeType::References, 1)],
                "{language:?}"
            );
        }
        // A double-quoted SQL name is an identifier; a single-quoted one is a literal.
        let symbols = vec![with_language(
            symbol(
                "target",
                "db",
                "orders",
                "db::orders",
                SymbolKind::DatabaseTable,
            ),
            Language::Sql,
        )];
        let report = resolve_symbol_edges(
            &[chunk_in(
                Language::Sql,
                "SELECT * FROM \"orders\";\nSELECT 'orders'; -- orders",
            )],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(
            targets(&report),
            vec![("db::orders".to_string(), GraphEdgeType::References, 1)]
        );
    }

    #[test]
    fn an_unclosed_single_line_literal_ends_with_its_line() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "util",
                "helper",
                "util::helper",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk_in(
                Language::TypeScript,
                "const s = 'unclosed\nhelper();",
            )],
            &symbols,
            &[],
            false,
            None,
        );
        assert_eq!(targets(&report), vec![call("util::helper", 2)]);
    }

    #[test]
    fn scip_availability_is_recorded_without_claiming_exactness() {
        let symbols = vec![
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            symbol(
                "target",
                "other",
                "target",
                "lib::target",
                SymbolKind::Function,
            ),
        ];
        let report = resolve_symbol_edges(
            &[chunk("c1", "entry", Some("caller"), "target();")],
            &symbols,
            &[],
            true,
            None,
        );
        let fact = report.analysis_facts.first().unwrap();
        assert_eq!(fact.confidence, Confidence::Medium);
        assert!(fact.message.contains("scip_available=true"));
    }

    fn reference(target: &str, line: u32) -> (String, GraphEdgeType, u32) {
        (target.to_string(), GraphEdgeType::References, line)
    }

    /// Symbols of `language`: the caller, and one function per name in another file, so each
    /// name is unique in the repository.
    fn unique_functions(language: Language, names: &[&str]) -> Vec<Symbol> {
        let mut symbols = vec![with_language(
            symbol("caller", "entry", "main", "app::main", SymbolKind::Function),
            language.clone(),
        )];
        for name in names {
            symbols.push(with_language(
                symbol(
                    &format!("fn-{name}"),
                    "util",
                    name,
                    &format!("util::{name}"),
                    SymbolKind::Function,
                ),
                language.clone(),
            ));
        }
        symbols
    }

    fn resolve_text(language: Language, text: &str, symbols: &[Symbol]) -> RegistryReport {
        resolve_symbol_edges(&[chunk_in(language, text)], symbols, &[], false, None)
    }

    /// `targets`, ordered within a line by target, since facts come in id order.
    fn line_targets(report: &RegistryReport) -> Vec<(String, GraphEdgeType, u32)> {
        let mut targets = targets(report);
        targets.sort_by(|a, b| (a.2, &a.0).cmp(&(b.2, &b.0)));
        targets
    }

    #[test]
    fn attribute_and_annotation_names_are_not_matched_by_name_alone() {
        let symbols = unique_functions(Language::Rust, &["test", "derive", "helper"]);
        let text = "#[test]\n#[derive(\n    Debug,\n    helper,\n)]\nfn check() { helper(); }";
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(line_targets(&report), vec![call("util::helper", 6)]);
        assert!(report.quality_notes.iter().any(|note| note
            .message
            .contains("caveat for `test` via unresolved: attribute or annotation name")));

        let mut symbols = unique_functions(Language::Java, &["Override", "Inject", "helper"]);
        let text = "@Override\npublic void run(@Inject Foo foo) { helper(); }\n@Retries.Raw";
        let report = resolve_text(Language::Java, text, &symbols);
        assert_eq!(line_targets(&report), vec![call("util::helper", 2)]);

        // A Java annotation names a type, so a repository annotation type is its target.
        symbols.push(with_language(
            symbol(
                "retries",
                "retries",
                "Retries",
                "org::Retries",
                SymbolKind::Class,
            ),
            Language::Java,
        ));
        let report = resolve_text(Language::Java, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![call("util::helper", 2), reference("org::Retries", 3)]
        );

        // A Python `@` that does not start its line multiplies matrices.
        let symbols = unique_functions(Language::Python, &["cached", "weights"]);
        let text = "@cached\ndef f(x):\n    return x @ weights";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(line_targets(&report), vec![reference("util::weights", 3)]);
    }

    #[test]
    fn an_attribute_still_resolves_through_its_import() {
        let symbols = unique_functions(Language::Rust, &["traced"]);
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Rust, "#[traced]\nfn run() {}")],
            &symbols,
            &[import_resolution("entry", "crate::util::traced", "util")],
            false,
            None,
        );
        let fact = report
            .analysis_facts
            .first()
            .expect("an import-backed fact");
        assert_eq!(fact.target, "util::traced");
        assert!(fact.source.ends_with("direct-import"));
    }

    #[test]
    fn member_access_is_not_matched_by_name_alone() {
        let symbols = unique_functions(Language::Rust, &["contains", "expect", "helper"]);
        let text = "let found = items.contains(&x).then(|| 1).expect(\"x\");\nlet y = cfg\n    .contains(1);\nhelper(found);";
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(line_targets(&report), vec![call("util::helper", 4)]);
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.message.contains(
                "caveat for `contains` via unresolved: member access without receiver evidence"
            )));

        // The same member resolves where the file imports the module that defines it.
        let report = resolve_symbol_edges(
            &[chunk_in(Language::Rust, "items.contains(&x)")],
            &symbols,
            &[import_resolution("entry", "crate::util::contains", "util")],
            false,
            None,
        );
        assert_eq!(line_targets(&report), vec![call("util::contains", 1)]);

        let symbols = unique_functions(Language::TypeScript, &["render"]);
        let report = resolve_text(
            Language::TypeScript,
            "view?.render(); this.render();",
            &symbols,
        );
        assert_eq!(line_targets(&report), vec![]);
    }

    #[test]
    fn field_names_match_only_fields() {
        let symbols = unique_functions(Language::Rust, &["name", "limit", "value"]);
        let text = "let Options { name, .. } = options;\nlet query = Query {\n    limit,\n    name: value,\n};";
        let report = resolve_text(Language::Rust, text, &symbols);
        // `value` is the one name here used as a value.
        assert_eq!(line_targets(&report), vec![reference("util::value", 4)]);

        // A parameter is a field-like name too, and a function of that name is not its target.
        let report = resolve_text(Language::Rust, "fn f(limit: usize) {}", &symbols);
        assert_eq!(line_targets(&report), vec![]);

        // Where the repository has a field of that name, the field is the target.
        let mut with_field = symbols.clone();
        with_field.push(with_language(
            symbol(
                "field-limit",
                "opts",
                "limit",
                "opts::Query::limit",
                SymbolKind::Field,
            ),
            Language::Rust,
        ));
        with_field.retain(|symbol| symbol.id.0 != "fn-limit");
        let report = resolve_text(Language::Rust, "Query { limit: 1 }", &with_field);
        assert_eq!(
            line_targets(&report),
            vec![reference("opts::Query::limit", 1)]
        );

        // A block is not a struct literal: `if ready { value }` uses `value`.
        let report = resolve_text(Language::Rust, "if ready { value } else { 0 }", &symbols);
        assert_eq!(line_targets(&report), vec![reference("util::value", 1)]);

        let symbols = unique_functions(Language::Python, &["timeout", "retries"]);
        let report = resolve_text(Language::Python, "connect(timeout=retries)", &symbols);
        assert_eq!(line_targets(&report), vec![reference("util::retries", 1)]);

        let symbols = unique_functions(Language::TypeScript, &["render", "view"]);
        let report = resolve_text(
            Language::TypeScript,
            "const o = { render: view };",
            &symbols,
        );
        assert_eq!(line_targets(&report), vec![reference("util::view", 1)]);
    }

    #[test]
    fn a_name_the_chunk_binds_locally_is_not_matched_by_name_alone() {
        let symbols = unique_functions(Language::Rust, &["path", "token", "entry", "helper"]);
        let text = "helper(path);\nlet path = dir.join(\"x\");\nhelper(path);\nitems.iter().map(|(token, _)| token.len());\nfor entry in list { entry.touch(); }";
        let report = resolve_text(Language::Rust, text, &symbols);
        // Before its `let`, `path` is not yet the local.
        assert_eq!(
            line_targets(&report),
            vec![
                call("util::helper", 1),
                reference("util::path", 1),
                call("util::helper", 3),
            ]
        );

        // A parameter is a local of the function body.
        let report = resolve_text(
            Language::Rust,
            "fn run(mut path: PathBuf) {\n    helper(&mut path);\n}",
            &symbols,
        );
        assert_eq!(line_targets(&report), vec![call("util::helper", 2)]);

        let symbols = unique_functions(Language::Python, &["config", "item", "handle", "err"]);
        let text = "def run(config, *handle):\n    for item in config:\n        print(item, handle)\n    try:\n        pass\n    except Exception as err:\n        log(err)";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(line_targets(&report), vec![]);

        let symbols = unique_functions(Language::TypeScript, &["state", "load"]);
        let report = resolve_text(
            Language::TypeScript,
            "const { state } = store;\nload(state);",
            &symbols,
        );
        assert_eq!(line_targets(&report), vec![call("util::load", 2)]);

        // A Java call is never of a local variable.
        let symbols = unique_functions(Language::Java, &["count"]);
        let report = resolve_text(
            Language::Java,
            "int count = 0;\nreturn count + count();",
            &symbols,
        );
        assert_eq!(line_targets(&report), vec![call("util::count", 2)]);
    }

    #[test]
    fn python_f_string_fields_are_read_as_code() {
        let symbols = unique_functions(
            Language::Python,
            &["helper", "width", "hidden", "value", "key", "spec_text"],
        );
        let text = "a = f\"{helper(1)} and {{hidden}}\"\nb = f'{value!r:>{width}} {value:spec_text}'\nc = f\"{d['key']}\" + rf\"\\{value}\"";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![
                call("util::helper", 1),
                reference("util::value", 2),
                reference("util::width", 2),
                reference("util::value", 3),
            ]
        );

        // A triple-quoted f-string's fields span its lines; an unclosed one-line f-string ends
        // with its line.
        let text = "doc = f\"\"\"\n{helper()} {{hidden}}\n\"\"\"\nx = f\"{value\nhelper()";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![
                call("util::helper", 2),
                reference("util::value", 4),
                call("util::helper", 5),
            ]
        );
    }

    #[test]
    fn a_path_matches_by_name_only_a_symbol_it_leads_to() {
        let mut symbols = unique_functions(Language::Rust, &["take", "Document"]);
        symbols.push(with_language(
            symbol(
                "open-index",
                "store",
                "open_repo_index",
                "crates::open-kioku-store::src::lib::open_repo_index",
                SymbolKind::Method,
            ),
            Language::Rust,
        ));
        symbols.push(with_language(
            symbol(
                "store-type",
                "store",
                "SqliteStore",
                "crates::open-kioku-store::src::lib::SqliteStore",
                SymbolKind::Class,
            ),
            Language::Rust,
        ));
        symbols
            .iter_mut()
            .find(|symbol| symbol.id.0 == "open-index")
            .expect("the method")
            .parent_symbol_id = Some(SymbolId::new("store-type"));
        let text = "let parts = std::mem::take(&mut parts);\nlet doc: roxmltree::Document = parse();\nlet kind = SourceKind::Document;\nlet store = SqliteStore::open_repo_index(dir);\nlet same = open_kioku_store::SqliteStore::open_repo_index(dir);\nlet local = crate::util::take(1);";
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![
                reference("crates::open-kioku-store::src::lib::SqliteStore", 4),
                call("crates::open-kioku-store::src::lib::open_repo_index", 4),
                reference("crates::open-kioku-store::src::lib::SqliteStore", 5),
                call("crates::open-kioku-store::src::lib::open_repo_index", 5),
                call("util::take", 6),
            ]
        );
        assert!(report.quality_notes.iter().any(|note| note.message.contains(
            "caveat for `take` via unresolved: the name's path or import leads outside the repository"
        )));
    }

    /// The registry's report over `text` with a resolver import model holding `imports`, each a
    /// `(local name, source module)` of the chunk's file that resolved to nothing.
    fn resolve_with_imports(
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str)],
    ) -> RegistryReport {
        resolve_with_imports_in(Language::Rust, text, symbols, imports)
    }

    fn resolve_with_imports_in(
        language: Language,
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str)],
    ) -> RegistryReport {
        resolve_in_repository(language, text, symbols, imports, &[], &[], &[])
    }

    /// `resolve_with_imports_in` over a repository of `files` (`(id, path)`) whose `go.mod` files
    /// declare `go_modules` (`(directory, module)`), where the resolver placed the chunk's imports
    /// as `resolutions` (`(import path, status)`) says.
    fn resolve_in_repository(
        language: Language,
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str)],
        files: &[(&str, &str)],
        go_modules: &[(&str, &str)],
        resolutions: &[(&str, ResolutionStatus)],
    ) -> RegistryReport {
        let imports = imports
            .iter()
            .map(|(local, source)| ("entry", *local, *source))
            .collect::<Vec<_>>();
        resolve_in_model(
            language,
            text,
            symbols,
            &imports,
            files,
            go_modules,
            resolutions,
            &[],
            &[],
        )
    }

    /// `resolve_in_repository` with `imports` of any file (`(file id, local name, source)`), the
    /// Go type aliases `type_aliases` and the packages files declare (`(file id, package)`).
    #[allow(clippy::too_many_arguments)]
    fn resolve_in_model(
        language: Language,
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str, &str)],
        files: &[(&str, &str)],
        go_modules: &[(&str, &str)],
        resolutions: &[(&str, ResolutionStatus)],
        type_aliases: &[TypeAliasSite],
        packages: &[(&str, &str)],
    ) -> RegistryReport {
        let packages = packages
            .iter()
            .map(|(file, name)| PackageDeclarationSite {
                file_id: FileId::new(*file),
                name: name.to_string(),
            })
            .collect::<Vec<_>>();
        let files = files
            .iter()
            .map(|(id, path)| File {
                id: FileId::new(*id),
                repository_id: open_kioku_core::RepositoryId::new("repo"),
                path: path.into(),
                // A repository of several languages: each file's own, by its extension.
                language: match path.rsplit('.').next() {
                    Some("java") => Language::Java,
                    Some("py") => Language::Python,
                    Some("go") => Language::Go,
                    _ => language.clone(),
                },
                size_bytes: 0,
                content_hash: String::new(),
                is_generated: false,
                is_vendor: false,
            })
            .collect::<Vec<_>>();
        let resolutions = resolutions
            .iter()
            .map(|(path, status)| ImportResolution {
                status: status.clone(),
                target_file: None,
                ..import_resolution("entry", path, "none")
            })
            .collect::<Vec<_>>();
        let mut repository = SemanticRepository::new();
        for (dir, module) in go_modules {
            repository
                .project
                .roots
                .push(open_kioku_semantic_model::ProjectRoot {
                    path: dir.into(),
                    language: Language::Go,
                    package_name: Some(module.to_string()),
                    source_roots: vec![dir.into()],
                    library_root: None,
                    cargo_targets: Default::default(),
                    cargo_manifest: None,
                });
        }
        for (file, local, source) in imports {
            repository
                .imports
                .by_file_local_name
                .entry((FileId::new(*file), local.to_string()))
                .or_default()
                .push(ImportBinding {
                    file_id: FileId::new(*file),
                    scope_id: ScopeId::new("entry:scope"),
                    local_name: local.to_string(),
                    imported_name: source
                        .rsplit(['.', ':'])
                        .next()
                        .unwrap_or(source)
                        .to_string(),
                    source_module: source.to_string(),
                    resolved_module: None,
                    target_file: None,
                    target_symbol: None,
                    origin: open_kioku_semantic_model::ImportOrigin::Unknown,
                    is_type_only: false,
                    is_glob: false,
                    evidence: Vec::new(),
                    rule: Default::default(),
                });
        }
        let (symbol_index, scopes, bindings, inheritance) = (
            SymbolIndex::default(),
            ScopeIndex::default(),
            BindingIndex::default(),
            InheritanceIndex::default(),
        );
        let model = RegistryScopeModel::new(
            &files,
            &repository,
            &symbol_index,
            &scopes,
            &bindings,
            &inheritance,
            type_aliases,
            &packages,
        );
        resolve_symbol_edges(
            &[chunk_in(language, text)],
            symbols,
            &resolutions,
            false,
            Some(&model),
        )
    }

    #[test]
    fn a_name_the_file_imports_from_elsewhere_is_not_the_repositorys_same_named_symbol() {
        let symbols = unique_functions(Language::Rust, &["Result", "Command", "File"]);
        let text = "fn run(f: &File) -> Result<()> {\n    Command::new(\"git\");\n}";
        let report = resolve_with_imports(
            text,
            &symbols,
            &[
                ("Result", "anyhow::Result"),
                ("Command", "std::process::Command"),
                // An alias binds another name: `File` is still the repository's.
                ("FsFile", "std::fs::File"),
            ],
        );
        assert_eq!(line_targets(&report), vec![reference("util::File", 1)]);
        assert!(report.quality_notes.iter().any(|note| note.message.contains(
            "caveat for `Result` via unresolved: the name's path or import leads outside the repository"
        )));

        // An unresolved import through the repository's own modules may be a re-export
        // (`use crate::evidence::Result;` of `pub use util::Result;`), so it keeps the match.
        let report = resolve_with_imports(
            text,
            &symbols,
            &[
                ("Result", "crate::evidence::Result"),
                ("Command", "util::Command"),
            ],
        );
        assert_eq!(
            line_targets(&report),
            vec![
                reference("util::File", 1),
                reference("util::Result", 1),
                reference("util::Command", 2),
            ]
        );

        // Without the import model the registry does not know what the file binds.
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(line_targets(&report).len(), 3);
    }

    fn symbol_in(
        language: Language,
        id: &str,
        name: &str,
        qualified: &str,
        kind: SymbolKind,
    ) -> Symbol {
        with_language(symbol(id, id, name, qualified, kind), language)
    }

    /// A Java symbol of the file `file`, qualified by its path as the parser qualifies it, and a
    /// member of the class `parent` when one is given.
    fn java_symbol(
        id: &str,
        file: &str,
        parent: Option<&str>,
        name: &str,
        qualified: &str,
        kind: SymbolKind,
    ) -> Symbol {
        let mut symbol = with_language(symbol(id, file, name, qualified, kind), Language::Java);
        symbol.parent_symbol_id = parent.map(SymbolId::new);
        symbol
    }

    /// The caller, and a class `Constants` declaring `ACCESS_KEY` in the file `constants` at
    /// `src/main/java/org/example/Constants.java`.
    fn java_constants() -> Vec<Symbol> {
        vec![
            java_symbol(
                "caller",
                "entry",
                None,
                "main",
                "src::main::java::org::example::app::App::main",
                SymbolKind::Method,
            ),
            java_symbol(
                "constants",
                "constants",
                None,
                "Constants",
                "src::main::java::org::example::Constants::Constants",
                SymbolKind::Class,
            ),
            java_symbol(
                "key",
                "constants",
                Some("constants"),
                "ACCESS_KEY",
                "src::main::java::org::example::Constants::ACCESS_KEY",
                SymbolKind::Field,
            ),
        ]
    }

    /// The registry's report over Java `text` in the file `entry`, which imports `imports`, in a
    /// repository of `files` (`(id, path, declared package)`) beside `entry` itself, at
    /// `src/main/java/org/example/app/App.java` in `package org.example.app;`.
    fn resolve_java(
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str)],
        files: &[(&str, &str, Option<&str>)],
    ) -> RegistryReport {
        resolve_java_from(
            "src/main/java/org/example/app/App.java",
            text,
            symbols,
            imports,
            files,
        )
    }

    /// `resolve_java` with `entry` at `entry_path`, in `package org.example.app;`.
    fn resolve_java_from(
        entry_path: &str,
        text: &str,
        symbols: &[Symbol],
        imports: &[(&str, &str)],
        files: &[(&str, &str, Option<&str>)],
    ) -> RegistryReport {
        let entry = [("entry", entry_path, Some("org.example.app"))];
        let files = entry.iter().chain(files).collect::<Vec<_>>();
        let paths = files
            .iter()
            .map(|(id, path, _)| (*id, *path))
            .collect::<Vec<_>>();
        let packages = files
            .iter()
            .filter_map(|(id, _, package)| Some((*id, (*package)?)))
            .collect::<Vec<_>>();
        let imports = imports
            .iter()
            .map(|(local, source)| ("entry", *local, *source))
            .collect::<Vec<_>>();
        resolve_in_model(
            Language::Java,
            text,
            symbols,
            &imports,
            &paths,
            &[],
            &[],
            &[],
            &packages,
        )
    }

    const MAVEN_CONSTANTS: (&str, &str, Option<&str>) = (
        "constants",
        "src/main/java/org/example/Constants.java",
        Some("org.example"),
    );

    #[test]
    fn a_java_static_import_of_a_repository_member_is_not_from_outside() {
        let symbols = java_constants();
        let text = "String key = ACCESS_KEY;";
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.example.Constants.ACCESS_KEY")],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Constants::ACCESS_KEY",
                1
            )]
        );

        // A static import from a library still leads outside the repository.
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static com.vendor.Keys.ACCESS_KEY")],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(line_targets(&report), vec![]);
    }

    #[test]
    fn a_java_static_import_of_a_library_class_under_the_repositorys_root_is_not_its_member() {
        let symbols = java_constants();
        let text = "String key = ACCESS_KEY;";
        // `org` is the repository's package root, but `Keys` is none of its classes.
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.example.Keys.ACCESS_KEY")],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(line_targets(&report), vec![]);
        assert!(report.quality_notes.iter().any(|note| note
            .message
            .contains("no registry candidate belongs to the class its static import names")));

        // Nor is a library's `Constants` in another package under that root the repository's.
        let report = resolve_java(
            text,
            &symbols,
            &[(
                "ACCESS_KEY",
                "static org.example.vendor.Constants.ACCESS_KEY",
            )],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(line_targets(&report), vec![]);

        // A nested class's member is qualified by the file's class and has the nested class
        // as its parent, which the top-level class encloses.
        let mut nested = symbols.clone();
        nested.push(java_symbol(
            "outer",
            "outer",
            None,
            "Outer",
            "src::main::java::org::example::Outer::Outer",
            SymbolKind::Class,
        ));
        nested.push(java_symbol(
            "inner",
            "outer",
            Some("outer"),
            "Inner",
            "src::main::java::org::example::Outer::Inner",
            SymbolKind::Class,
        ));
        nested.push(java_symbol(
            "nested-value",
            "outer",
            Some("inner"),
            "NESTED_VALUE",
            "src::main::java::org::example::Outer::NESTED_VALUE",
            SymbolKind::Field,
        ));
        let outer = (
            "outer",
            "src/main/java/org/example/Outer.java",
            Some("org.example"),
        );
        let report = resolve_java(
            "String value = NESTED_VALUE;",
            &nested,
            &[(
                "NESTED_VALUE",
                "static org.example.Outer.Inner.NESTED_VALUE",
            )],
            &[MAVEN_CONSTANTS, outer],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Outer::NESTED_VALUE",
                1
            )]
        );
        // The nested class alone, without the class enclosing it, names no class.
        let report = resolve_java(
            "String value = NESTED_VALUE;",
            &nested,
            &[("NESTED_VALUE", "static org.example.Inner.NESTED_VALUE")],
            &[MAVEN_CONSTANTS, outer],
        );
        assert_eq!(line_targets(&report), vec![]);

        // A receiver imported from a library is the library's class, whatever its name.
        let report = resolve_java(
            "String key = Constants.ACCESS_KEY;",
            &symbols,
            &[("Constants", "com.vendor.Constants")],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(line_targets(&report), vec![]);
        assert!(report.quality_notes.iter().any(|note| note.message.contains(
            "caveat for `ACCESS_KEY` via unresolved: the member's receiver is imported from outside the repository"
        )));

        // Beside a static import from a library, the repository's class still counts.
        let report = resolve_java(
            text,
            &symbols,
            &[
                ("ACCESS_KEY", "static com.vendor.Keys.ACCESS_KEY"),
                ("ACCESS_KEY", "static org.example.Constants.ACCESS_KEY"),
            ],
            &[MAVEN_CONSTANTS],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Constants::ACCESS_KEY",
                1
            )]
        );
    }

    #[test]
    fn a_java_static_import_names_its_class_by_the_package_its_file_declares() {
        // `src/Constants.java` declares `package org.example;`: its directory mirrors no package,
        // and no other path of the repository spells `org`.
        let flat = vec![
            java_symbol(
                "caller",
                "entry",
                None,
                "main",
                "src::App::main",
                SymbolKind::Method,
            ),
            java_symbol(
                "constants",
                "constants",
                None,
                "Constants",
                "src::Constants::Constants",
                SymbolKind::Class,
            ),
            java_symbol(
                "key",
                "constants",
                Some("constants"),
                "FLAT_KEY",
                "src::Constants::FLAT_KEY",
                SymbolKind::Field,
            ),
        ];
        let flat_constants = ("constants", "src/Constants.java", Some("org.example"));
        let text = "String key = FLAT_KEY;";
        let report = resolve_java(
            text,
            &flat,
            &[("FLAT_KEY", "static org.example.Constants.FLAT_KEY")],
            &[flat_constants],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference("src::Constants::FLAT_KEY", 1)]
        );

        // Another class of that package is still not `Constants`, and the caveat says so.
        let report = resolve_java(
            text,
            &flat,
            &[("FLAT_KEY", "static org.example.Keys.FLAT_KEY")],
            &[flat_constants],
        );
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(
            &report,
            "caveat for `FLAT_KEY` via unresolved: no registry candidate belongs to the class its static import names"
        ));

        // A directory that spells the import's package is not the package its file declares.
        let symbols = java_constants();
        let elsewhere = (
            "constants",
            "src/main/java/org/example/Constants.java",
            Some("org.other"),
        );
        let text = "String key = ACCESS_KEY;";
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.example.Constants.ACCESS_KEY")],
            &[elsewhere],
        );
        assert_eq!(line_targets(&report), vec![]);
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.other.Constants.ACCESS_KEY")],
            &[elsewhere],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Constants::ACCESS_KEY",
                1
            )]
        );

        // A file declaring no package is in the unnamed package, which no import names.
        let report = resolve_java(
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.example.Constants.ACCESS_KEY")],
            &[(
                "constants",
                "src/main/java/org/example/Constants.java",
                None,
            )],
        );
        assert_eq!(line_targets(&report), vec![]);
    }

    #[test]
    fn a_java_import_from_outside_every_declared_package_leads_outside_the_repository() {
        // A flat layout: `src/Widget.java` declares `package org.example;`, and no path of the
        // repository spells `org`.
        let symbols = vec![
            java_symbol(
                "caller",
                "entry",
                None,
                "f",
                "src::app::UsesLibrary::f",
                SymbolKind::Method,
            ),
            java_symbol(
                "widget",
                "widget",
                None,
                "Widget",
                "src::Widget::Widget",
                SymbolKind::Class,
            ),
            java_symbol(
                "build",
                "widget",
                Some("widget"),
                "build",
                "src::Widget::build",
                SymbolKind::Method,
            ),
            java_symbol(
                "mocks",
                "mocks",
                None,
                "Mocks",
                "src::Mocks::Mocks",
                SymbolKind::Class,
            ),
            java_symbol(
                "mock-thing",
                "mocks",
                Some("mocks"),
                "mockThing",
                "src::Mocks::mockThing",
                SymbolKind::Method,
            ),
        ];
        let files = [
            ("widget", "src/Widget.java", Some("org.example")),
            ("mocks", "src/Mocks.java", Some("org.example")),
        ];
        let resolve = |text: &str, imports: &[(&str, &str)]| {
            resolve_java_from("src/app/UsesLibrary.java", text, &symbols, imports, &files)
        };
        let text = "Widget w = Widget.build();";

        // A library's `Widget` shares only the `org` segment with the declared package.
        let report = resolve(text, &[("Widget", "org.apache.commons.Widget")]);
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(
            &report,
            "caveat for `build` via unresolved: the member's receiver is imported from outside the repository"
        ));
        let report = resolve(
            "Object o = mockThing();",
            &[("mockThing", "static org.mockito.Mockito.mockThing")],
        );
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(
            &report,
            "caveat for `mockThing` via unresolved: the name's path or import leads outside the repository"
        ));
        // A package is matched by whole segments: `org.examples` is not inside `org.example`.
        let report = resolve(text, &[("Widget", "org.examples.Widget")]);
        assert_eq!(line_targets(&report), vec![]);

        // An import from inside the declared package names the repository's class.
        let report = resolve(text, &[("Widget", "org.example.Widget")]);
        assert_eq!(
            line_targets(&report),
            vec![
                reference("src::Widget::Widget", 1),
                call("src::Widget::build", 1),
            ]
        );
    }

    #[test]
    fn another_languages_import_is_not_read_against_a_declared_java_package() {
        // `java/Acme.java` declares `package io.acme;`; Python's `io` is the standard library's.
        let python = |id: &str, file: &str, name: &str, qualified: &str, kind: SymbolKind| {
            let mut symbol = symbol_in(Language::Python, id, name, qualified, kind);
            symbol.file_id = FileId::new(file);
            symbol
        };
        let mut acme = java_symbol(
            "acme",
            "acme",
            None,
            "Acme",
            "java::Acme::Acme",
            SymbolKind::Class,
        );
        acme.language = Language::Java;
        let symbols = vec![
            python(
                "caller",
                "entry",
                "render",
                "py::app::render",
                SymbolKind::Function,
            ),
            python(
                "string-io",
                "buffers",
                "StringIO",
                "py::buffers::StringIO",
                SymbolKind::Class,
            ),
            acme,
        ];
        let files = [
            ("entry", "py/app.py"),
            ("buffers", "py/buffers.py"),
            ("acme", "java/Acme.java"),
        ];
        // Even a path inside the Java package is Python's own: only a Java import is read
        // against a Java package.
        for source in ["io", "io.StringIO", "io.acme.StringIO"] {
            let report = resolve_in_model(
                Language::Python,
                "return StringIO().getvalue()",
                &symbols,
                &[("entry", "StringIO", source)],
                &files,
                &[],
                &[],
                &[],
                &[("acme", "io.acme")],
            );
            assert_eq!(line_targets(&report), vec![], "{source}");
            assert!(
                has_note(
                    &report,
                    "caveat for `StringIO` via unresolved: the name's path or import leads outside the repository"
                ),
                "{source}: {report:?}"
            );
        }
    }

    /// A Go caller in `entry` and, for each `(id, file path, name)`, a function in that file,
    /// qualified by the file's directories and stem; with the files' `(id, path)`.
    fn go_symbols(entries: &[(&str, &str, &str)]) -> (Vec<Symbol>, Vec<(String, String)>) {
        let mut caller = symbol_in(
            Language::Go,
            "caller",
            "main",
            "cmd::main::main",
            SymbolKind::Function,
        );
        caller.file_id = FileId::new("entry");
        let mut symbols = vec![caller];
        let mut files = vec![("entry".to_string(), "cmd/main/main.go".to_string())];
        for (id, path, name) in entries {
            let stem = path.strip_suffix(".go").unwrap_or(path).replace('/', "::");
            let qualified = format!("{stem}::{name}");
            let mut symbol = symbol_in(Language::Go, id, name, &qualified, SymbolKind::Function);
            symbol.file_id = FileId::new(*id);
            symbols.push(symbol);
            files.push((id.to_string(), path.to_string()));
        }
        (symbols, files)
    }

    /// The registry's report over Go `text` in a repository of `entries` whose `go.mod` files
    /// declare `modules` (`(directory, module)`).
    fn resolve_go(
        text: &str,
        entries: &[(&str, &str, &str)],
        imports: &[(&str, &str)],
        modules: &[(&str, &str)],
        resolutions: &[(&str, ResolutionStatus)],
    ) -> RegistryReport {
        let (symbols, files) = go_symbols(entries);
        let files = files
            .iter()
            .map(|(id, path)| (id.as_str(), path.as_str()))
            .collect::<Vec<_>>();
        resolve_in_repository(
            Language::Go,
            text,
            &symbols,
            imports,
            &files,
            modules,
            resolutions,
        )
    }

    fn has_note(report: &RegistryReport, text: &str) -> bool {
        report
            .quality_notes
            .iter()
            .any(|note| note.message.contains(text))
    }

    #[test]
    fn a_go_receiver_imported_from_another_module_is_not_the_repositorys_package() {
        let app = [("", "example.com/app")];
        let text = "func New(ui cli.Ui) {}\nid := uuid.Generate()";
        let imports = [
            ("cli", "example.com/vendor/cli"),
            ("uuid", "example.com/app/lib/uuid"),
        ];
        let outside = "caveat for `Ui` via unresolved: the member's receiver is imported from outside the repository";
        // Another module's `cli` is neither `command/cli` nor a top-level `cli`.
        for dir in ["command/cli", "cli"] {
            let entries = [
                ("ui", &*format!("{dir}/cli.go"), "Ui"),
                ("gen", "lib/uuid/uuid.go", "Generate"),
            ];
            let report = resolve_go(text, &entries, &imports, &app, &[]);
            assert_eq!(
                line_targets(&report),
                vec![call("lib::uuid::uuid::Generate", 2)]
            );
            assert!(has_note(&report, outside));
        }

        // A vendored copy is the package.
        let entries = [("ui", "vendor/example.com/vendor/cli/cli.go", "Ui")];
        let report = resolve_go(text, &entries, &imports, &app, &[]);
        assert_eq!(
            line_targets(&report),
            vec![reference("vendor::example.com::vendor::cli::cli::Ui", 1)]
        );

        // The standard library is outside every module, and with no module declared the
        // resolver's standard-library verdict still says so.
        let entries = [("ctx", "context/context.go", "Background")];
        let text = "ctx := context.Background()";
        let imports = [("context", "context")];
        let report = resolve_go(text, &entries, &imports, &app, &[]);
        assert_eq!(line_targets(&report), vec![]);
        let builtin = [("context", ResolutionStatus::Builtin)];
        let report = resolve_go(text, &entries, &imports, &[], &builtin);
        assert_eq!(line_targets(&report), vec![]);
    }

    #[test]
    fn a_go_receiver_names_the_package_its_whole_import_path_leads_to() {
        // `ledger` also names the file `billing/ledgerutil/ledger.go`, and `worker` the
        // directory `client/worker`; neither is the package the import leads to.
        let entries = [
            ("util-entry", "billing/ledgerutil/ledger.go", "Entry"),
            ("client-start", "client/worker/worker.go", "Start"),
            ("stop", "worker/worker.go", "Stop"),
            ("version", "version.go", "Version"),
        ];
        let text = "e := ledger.Entry{}\nworker.Start()\nworker.Stop()\nw.Stop()\napp.Version()";
        let imports = [
            ("ledger", "example.com/app/billing/ledger"),
            ("worker", "example.com/app/worker"),
            // An alias names the package under another name.
            ("w", "example.com/app/worker"),
            // The module path itself is the root package.
            ("app", "example.com/app"),
        ];
        let report = resolve_go(text, &entries, &imports, &[("", "example.com/app")], &[]);
        assert_eq!(
            line_targets(&report),
            vec![
                call("worker::worker::Stop", 3),
                call("worker::worker::Stop", 4),
                call("version::Version", 5),
            ]
        );
        assert!(has_note(
            &report,
            "caveat for `Entry` via unresolved: no registry candidate is in the package the receiver's import names"
        ));

        // Where no module is declared, the longest directory of the repository the path ends
        // with stands in for the package.
        let entries = [
            ("entry", "internal/ledger/ledger.go", "Entry"),
            ("total", "ledger/ledger.go", "Total"),
        ];
        let report = resolve_go(
            "e := ledger.Entry{}\nt := ledger.Total()",
            &entries,
            &[("ledger", "example.com/app/internal/ledger")],
            &[],
            &[(
                "example.com/app/internal/ledger",
                ResolutionStatus::ExternalPackage,
            )],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference("internal::ledger::ledger::Entry", 1)]
        );
    }

    #[test]
    fn a_go_import_under_a_declared_module_names_its_package_not_a_longer_directory() {
        // The path below the module is the directory, though the longer `server/config` also
        // ends the path.
        let entries = [
            ("load", "config/config.go", "LoadSettings"),
            ("internal", "server/config/config.go", "Internal"),
        ];
        let report = resolve_go(
            "config.LoadSettings()",
            &entries,
            &[("config", "example.com/server/config")],
            &[("", "example.com/server")],
            &[],
        );
        assert_eq!(
            line_targets(&report),
            vec![call("config::config::LoadSettings", 1)]
        );
    }

    #[test]
    fn a_go_module_path_without_a_dot_is_the_repositorys_not_the_standard_librarys() {
        // The resolver takes every path without a dot for the standard library's.
        let entries = [("save", "internal/store/save.go", "SaveRecord")];
        let report = resolve_go(
            "store.SaveRecord(id)",
            &entries,
            &[("store", "myapp/internal/store")],
            &[("", "myapp")],
            &[("myapp/internal/store", ResolutionStatus::Builtin)],
        );
        assert_eq!(
            line_targets(&report),
            vec![call("internal::store::save::SaveRecord", 1)]
        );
    }

    #[test]
    fn a_go_module_declared_twice_is_the_importers_own_or_else_the_shallowest() {
        let entries = [
            ("own", "cmd/internal/util/util.go", "Helper"),
            ("root", "internal/util/util.go", "Other"),
        ];
        let text = "util.Helper()\nutil.Other()";
        let imports = [("util", "example.com/app/internal/util")];
        // A copy of the root's `go.mod` below the importer's directory is not its module.
        let report = resolve_go(
            text,
            &entries,
            &imports,
            &[
                ("", "example.com/app"),
                ("testdata/copy", "example.com/app"),
            ],
            &[],
        );
        assert_eq!(
            line_targets(&report),
            vec![call("internal::util::util::Other", 2)]
        );
        // One that holds the importer is.
        let report = resolve_go(
            text,
            &entries,
            &imports,
            &[("cmd", "example.com/app"), ("", "example.com/app")],
            &[],
        );
        assert_eq!(
            line_targets(&report),
            vec![call("cmd::internal::util::util::Helper", 1)]
        );
    }

    #[test]
    fn a_go_mod_the_go_command_ignores_declares_no_module() {
        let app = ("", "example.com/app");
        // `testdata` and `_`-prefixed directories are not part of the build, whatever module
        // their `go.mod` names and whatever Go files they hold.
        let entries = [
            ("x-func", "internal/x/a.go", "XFunc"),
            ("fixture", "internal/x/testdata/fixture.go", "Fixture"),
        ];
        let report = resolve_go(
            "x.XFunc()",
            &entries,
            &[("x", "example.com/app/internal/x")],
            &[app, ("internal/x/testdata", "example.com/app/internal/x")],
            &[],
        );
        assert_eq!(
            line_targets(&report),
            vec![call("internal::x::a::XFunc", 1)]
        );

        let entries = [
            ("y-func", "pkg/y/a.go", "YFunc"),
            ("stub", "_scratch/y/stub.go", "Stub"),
        ];
        let report = resolve_go(
            "y.YFunc()",
            &entries,
            &[("y", "example.com/app/pkg/y")],
            &[app, ("_scratch", "example.com/app/pkg")],
            &[],
        );
        assert_eq!(line_targets(&report), vec![call("pkg::y::a::YFunc", 1)]);
    }

    #[test]
    fn a_go_module_holding_no_package_at_the_path_gives_way_to_one_that_does() {
        let entries = [("y-func", "pkg/y/a.go", "YFunc")];
        let report = resolve_go(
            "y.YFunc()",
            &entries,
            &[("y", "example.com/app/pkg/y")],
            &[("", "example.com/app"), ("tools", "example.com/app/pkg")],
            &[],
        );
        assert_eq!(line_targets(&report), vec![call("pkg::y::a::YFunc", 1)]);
    }

    #[test]
    fn a_nested_go_module_places_its_packages_below_its_own_directory() {
        // Whatever module the root declares: the resolver places the root module's paths and
        // leaves this one outside, reading its rest from the repository's root.
        let entries = [
            ("gen", "tools/gen/gen.go", "GenerateCode"),
            ("helper", "internal/util/util.go", "Helper"),
        ];
        let report = resolve_go(
            "util.Helper()\ngen.GenerateCode()",
            &entries,
            &[
                ("util", "example.com/app/internal/util"),
                ("gen", "example.com/tools/gen"),
            ],
            &[("", "example.com/app"), ("tools", "example.com/tools")],
            &[
                (
                    "example.com/app/internal/util",
                    ResolutionStatus::Ambiguous { candidates: 2 },
                ),
                ("example.com/tools/gen", ResolutionStatus::ExternalPackage),
            ],
        );
        assert_eq!(
            line_targets(&report),
            vec![
                call("internal::util::util::Helper", 1),
                call("tools::gen::gen::GenerateCode", 2),
            ]
        );
    }

    /// The Go files of the alias tests, `(id, path)`: the caller's and the `billing/ledger`,
    /// `billing/mid`, `billing/store` and `audit` packages.
    const ALIAS_FILES: [(&str, &str); 6] = [
        ("entry", "cmd/main/main.go"),
        ("ledger-aliases", "billing/ledger/aliases.go"),
        ("ledger-record", "billing/ledger/record.go"),
        ("mid", "billing/mid/mid.go"),
        ("store", "billing/store/store.go"),
        ("audit", "audit/entry.go"),
    ];

    /// `(id, file id, name, alias target)` of a top-level Go type; one with an alias target is a
    /// type alias of `(package qualifier, name)`, `(None, None)` for a type named by no name.
    type GoTypeEntry<'e> = (
        &'e str,
        &'e str,
        &'e str,
        Option<(Option<&'e str>, Option<&'e str>)>,
    );

    /// A Go caller in `entry` and, for each entry, a Go type of its file, qualified as the parser
    /// qualifies it.
    fn go_types(entries: &[GoTypeEntry<'_>]) -> (Vec<Symbol>, Vec<TypeAliasSite>) {
        let mut caller = symbol_in(
            Language::Go,
            "caller",
            "main",
            "cmd::main::main::main",
            SymbolKind::Function,
        );
        caller.file_id = FileId::new("entry");
        let mut symbols = vec![caller];
        let mut aliases = Vec::new();
        for (id, file, name, alias) in entries {
            let (_, path) = ALIAS_FILES
                .iter()
                .find(|(file_id, _)| file_id == file)
                .expect("alias test files list every file");
            let stem = path.strip_suffix(".go").unwrap_or(path).replace('/', "::");
            let mut symbol = symbol_in(
                Language::Go,
                id,
                name,
                &format!("{stem}::{name}"),
                SymbolKind::Class,
            );
            symbol.file_id = FileId::new(*file);
            if let Some((package, target)) = alias {
                let written = match (package, target) {
                    (Some(package), Some(target)) => format!("{package}.{target}"),
                    (None, Some(target)) => target.to_string(),
                    _ => "[]byte".to_string(),
                };
                symbol.signature = Some(format!("type {name} = {written}"));
                aliases.push(TypeAliasSite {
                    symbol_id: symbol.id.clone(),
                    target_package: package.map(str::to_string),
                    target_name: target.map(str::to_string),
                });
            }
            symbols.push(symbol);
        }
        (symbols, aliases)
    }

    fn resolve_go_aliases(
        text: &str,
        entries: &[GoTypeEntry<'_>],
        imports: &[(&str, &str, &str)],
    ) -> RegistryReport {
        let (symbols, aliases) = go_types(entries);
        resolve_in_model(
            Language::Go,
            text,
            &symbols,
            imports,
            &ALIAS_FILES,
            &[("", "example.com/app")],
            &[],
            &aliases,
            &[],
        )
    }

    #[test]
    fn a_go_member_reached_through_an_alias_is_the_type_the_alias_stands_for() {
        let entries = [
            (
                "alias-entry",
                "ledger-aliases",
                "Entry",
                Some((Some("store"), Some("Entry"))),
            ),
            (
                "alias-batch",
                "ledger-aliases",
                "Batch",
                Some((Some("mid"), Some("Batch"))),
            ),
            (
                "alias-local",
                "ledger-aliases",
                "Local",
                Some((None, Some("Record"))),
            ),
            ("record", "ledger-record", "Record", None),
            // An alias of an alias, placed through its own file's import.
            (
                "mid-batch",
                "mid",
                "Batch",
                Some((Some("store"), Some("Batch"))),
            ),
            ("store-entry", "store", "Entry", None),
            ("store-batch", "store", "Batch", None),
        ];
        let imports = [
            ("entry", "ledger", "example.com/app/billing/ledger"),
            ("ledger-aliases", "store", "example.com/app/billing/store"),
            ("ledger-aliases", "mid", "example.com/app/billing/mid"),
            ("mid", "store", "example.com/app/billing/store"),
        ];
        let report = resolve_go_aliases(
            "e := ledger.Entry{}\nb := ledger.Batch{}\nl := ledger.Local{}",
            &entries,
            &imports,
        );
        assert_eq!(
            line_targets(&report),
            vec![
                reference("billing::store::store::Entry", 1),
                reference("billing::store::store::Batch", 2),
                reference("billing::ledger::record::Record", 3),
            ]
        );

        // An alias and the type it stands for are one candidate for a name.
        let report = resolve_go_aliases("func f(e Entry) {}", &entries[..1], &imports);
        assert_eq!(line_targets(&report), vec![]);
        let entries = [entries[0], entries[5]];
        let report = resolve_go_aliases("func f(e Entry) {}", &entries, &imports);
        assert_eq!(
            line_targets(&report),
            vec![reference("billing::store::store::Entry", 1)]
        );

        // An alias in another package is no candidate for a member its receiver places: the
        // alias `ledger.Entry` does not make `store.Entry` ambiguous.
        let report = resolve_go_aliases(
            "e := store.Entry{}",
            &entries,
            &[("entry", "store", "example.com/app/billing/store")],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference("billing::store::store::Entry", 1)]
        );
        let renamed = [
            (
                "alias-entry",
                "ledger-aliases",
                "Entry",
                Some((None, Some("Record"))),
            ),
            ("record", "ledger-record", "Record", None),
            ("store-entry", "store", "Entry", None),
        ];
        let report = resolve_go_aliases(
            "e := store.Entry{}",
            &renamed,
            &[("entry", "store", "example.com/app/billing/store")],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference("billing::store::store::Entry", 1)]
        );

        // A same-named type outside the alias's target package is still another candidate.
        let entries = [
            entries[0],
            entries[1],
            ("audit-entry", "audit", "Entry", None),
        ];
        let report = resolve_go_aliases("e := ledger.Entry{}", &entries, &imports);
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(
            &report,
            "caveat for `Entry` via unique-project-name: 2 candidates matched via unique-project-name"
        ));
    }

    #[test]
    fn a_go_alias_whose_target_is_not_placed_resolves_to_nothing_and_its_caveat_names_it() {
        let entries = [
            ("alias-raw", "ledger-aliases", "Raw", Some((None, None))),
            (
                "alias-ui",
                "ledger-aliases",
                "Ui",
                Some((Some("cli"), Some("Ui"))),
            ),
            (
                "alias-pair",
                "ledger-aliases",
                "Pair",
                Some((Some("store"), Some("Twice"))),
            ),
            // Declared once per build constraint.
            ("store-twice-a", "store", "Twice", None),
            ("store-twice-b", "store", "Twice", None),
            // Not the other module's `Ui` its alias names.
            ("audit-ui", "audit", "Ui", None),
        ];
        let imports = [
            ("entry", "ledger", "example.com/app/billing/ledger"),
            ("ledger-aliases", "store", "example.com/app/billing/store"),
            ("ledger-aliases", "cli", "example.com/vendor/cli"),
        ];
        let report = resolve_go_aliases(
            "var r ledger.Raw\nvar u ledger.Ui\nvar p ledger.Pair",
            &entries,
            &imports,
        );
        assert_eq!(line_targets(&report), vec![]);
        for (alias, written) in [("Raw", "[]byte"), ("Pair", "store.Twice")] {
            let caveat = format!(
                "caveat for `{alias}` via unique-project-name: `billing::ledger::aliases::{alias}` is a Go type alias of `{written}`, which the registry could not place at one repository type"
            );
            assert!(has_note(&report, &caveat), "{caveat}: {report:?}");
        }
        // `audit.Ui` is another candidate, as a name match reads it.
        assert!(has_note(
            &report,
            "caveat for `Ui` via unique-project-name: 2 candidates matched via unique-project-name"
        ));
        let report = resolve_go_aliases("var u ledger.Ui", &entries[..2], &imports);
        assert!(has_note(
            &report,
            "caveat for `Ui` via unique-project-name: `billing::ledger::aliases::Ui` is a Go type alias of `cli.Ui`, which the registry could not place at one repository type"
        ));
    }

    /// The registry's report over Go `text` in the file `entry` at `entry_path`, declaring
    /// `entry_package`, beside a package `store` whose directory also holds a `_test.go` file of
    /// its own package and one of the external test package `store_test`.
    fn resolve_beside_external_test(
        text: &str,
        entry_path: &str,
        entry_package: &str,
    ) -> RegistryReport {
        resolve_beside_external_test_with(text, entry_path, entry_package, &[])
    }

    /// `resolve_beside_external_test` with the symbols `others` of files outside the Go model.
    fn resolve_beside_external_test_with(
        text: &str,
        entry_path: &str,
        entry_package: &str,
        others: &[Symbol],
    ) -> RegistryReport {
        let go = |id: &str, file: &str, name: &str, qualified: &str, kind: SymbolKind| {
            let mut symbol = symbol_in(Language::Go, id, name, qualified, kind);
            symbol.file_id = FileId::new(file);
            symbol
        };
        let mut alias = go(
            "ext-entry",
            "store-ext",
            "Entry",
            "store::store_test::Entry",
            SymbolKind::Class,
        );
        alias.signature = Some("type Entry = audit.Record".into());
        let mut symbols = vec![
            go(
                "caller",
                "entry",
                "Run",
                "cmd::main::Run",
                SymbolKind::Function,
            ),
            go(
                "store-entry",
                "store",
                "Entry",
                "store::store::Entry",
                SymbolKind::Class,
            ),
            alias,
            go(
                "ext-helper",
                "store-ext",
                "ExtHelper",
                "store::store_test::ExtHelper",
                SymbolKind::Function,
            ),
            go(
                "for-test",
                "store-int",
                "ExportedForTest",
                "store::export_test::ExportedForTest",
                SymbolKind::Function,
            ),
            go(
                "record",
                "audit",
                "Record",
                "audit::audit::Record",
                SymbolKind::Class,
            ),
        ];
        symbols.extend_from_slice(others);
        let files = [
            ("entry", entry_path),
            ("store", "store/store.go"),
            ("store-ext", "store/store_test.go"),
            ("store-int", "store/export_test.go"),
            ("audit", "audit/audit.go"),
        ];
        let packages = [
            ("entry", entry_package),
            ("store", "store"),
            ("store-ext", "store_test"),
            ("store-int", "store"),
            ("audit", "audit"),
        ];
        let aliases = [TypeAliasSite {
            symbol_id: SymbolId::new("ext-entry"),
            target_package: Some("audit".into()),
            target_name: Some("Record".into()),
        }];
        resolve_in_model(
            Language::Go,
            text,
            &symbols,
            &[
                ("entry", "store", "example.com/app/store"),
                ("store-ext", "audit", "example.com/app/audit"),
            ],
            &files,
            &[("", "example.com/app")],
            &[],
            &aliases,
            &packages,
        )
    }

    #[test]
    fn a_go_external_test_packages_declarations_are_not_the_package_it_tests() {
        // `store_test` shares `store/` with `store`, but its alias is not `store.Entry`.
        let text = "e := store.Entry{}\nExtHelper()\nstore.ExportedForTest()";
        let report = resolve_beside_external_test(text, "cmd/main.go", "main");
        assert_eq!(
            line_targets(&report),
            vec![reference("store::store::Entry", 1)]
        );
        assert!(!has_note(&report, "candidates matched"), "{report:?}");

        // Its own files still reach its declarations, and import the package it tests, whose
        // `_test.go` files are of that package.
        let report = resolve_beside_external_test(text, "store/other_test.go", "store_test");
        assert_eq!(
            line_targets(&report),
            vec![
                reference("store::store::Entry", 1),
                call("store::store_test::ExtHelper", 2),
                call("store::export_test::ExportedForTest", 3),
            ]
        );

        // A `_test.go` file of the package itself is not in the external test package.
        let report = resolve_beside_external_test("ExtHelper()", "store/more_test.go", "store");
        assert_eq!(line_targets(&report), vec![]);
        // Nor is a file whose clause alone ends in `_test`: only a `_test.go` file can be.
        let report = resolve_beside_external_test("ExtHelper()", "store/helper.go", "store_test");
        assert_eq!(line_targets(&report), vec![]);
    }

    #[test]
    fn a_go_test_files_declarations_are_not_named_from_another_directory() {
        // Go compiles a `_test.go` file only into the tests of its own directory's package.
        let text = "ExportedForTest()\nstore.ExportedForTest()";
        let report = resolve_beside_external_test(text, "cmd/main.go", "main");
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(
            &report,
            "caveat for `ExportedForTest` via unresolved: no registry candidate is in the package the receiver's import names"
        ));
        let report = resolve_beside_external_test(text, "cmd/main_test.go", "main");
        assert_eq!(line_targets(&report), vec![]);

        // A declaration it cannot name does not stand in for the token's language either: beside
        // the one JavaScript `ExportedForTest`, the call is of no candidate.
        let mut script = symbol(
            "script",
            "script",
            "ExportedForTest",
            "ui::app::ExportedForTest",
            SymbolKind::Function,
        );
        script.language = Language::JavaScript;
        let report = resolve_beside_external_test_with(
            "ExportedForTest()",
            "cmd/main.go",
            "main",
            &[script],
        );
        assert_eq!(line_targets(&report), vec![]);

        // Nor does ruling it out make another package's declaration of the name the one: the
        // name is still declared twice, as beside any declaration a use cannot reach.
        let mut audit = symbol(
            "audit-helper",
            "audit",
            "ExportedForTest",
            "audit::audit::ExportedForTest",
            SymbolKind::Function,
        );
        audit.language = Language::Go;
        let report =
            resolve_beside_external_test_with("ExportedForTest()", "cmd/main.go", "main", &[audit]);
        assert_eq!(line_targets(&report), vec![]);
        assert!(has_note(&report, "2 candidates matched"), "{report:?}");

        // Beside it, in the package itself, the declaration is named.
        let report =
            resolve_beside_external_test("ExportedForTest()", "store/more_test.go", "store");
        assert_eq!(
            line_targets(&report),
            vec![call("store::export_test::ExportedForTest", 1)]
        );
    }

    #[test]
    fn a_member_matches_by_name_where_its_receiver_names_the_symbols_place() {
        let symbols = vec![
            symbol_in(
                Language::Go,
                "caller",
                "main",
                "app::main",
                SymbolKind::Function,
            ),
            symbol_in(
                Language::Go,
                "new-entry",
                "NewEntry",
                "billing::ledger::entries::NewEntry",
                SymbolKind::Function,
            ),
        ];
        let text = "id := ledger.NewEntry(name)\nother := entry.NewEntry(name)";
        let report = resolve_text(Language::Go, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![call("billing::ledger::entries::NewEntry", 1)]
        );

        let symbols = vec![
            symbol_in(
                Language::Java,
                "caller",
                "main",
                "app::main",
                SymbolKind::Function,
            ),
            symbol_in(
                Language::Java,
                "key",
                "ACCESS_KEY",
                "src::main::java::org::example::Constants::ACCESS_KEY",
                SymbolKind::Field,
            ),
        ];
        let text = "String a = Constants.ACCESS_KEY;\nString b = config.ACCESS_KEY;\nimport org.example.Constants.ACCESS_KEY;\nString c = example.ACCESS_KEY;";
        let report = resolve_text(Language::Java, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Constants::ACCESS_KEY",
                1
            )]
        );

        // A receiver that is itself a member, a call result or `self` says nothing.
        let symbols = vec![
            symbol_in(
                Language::Python,
                "caller",
                "main",
                "app::main",
                SymbolKind::Function,
            ),
            symbol_in(
                Language::Python,
                "fmt",
                "format_size",
                "pkg::utils::misc::format_size",
                SymbolKind::Function,
            ),
        ];
        let text = "a = misc.format_size(1)\nb = self.misc.format_size(1)\nc = misc().format_size(1)\nimport pkg.misc.format_size";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![call("pkg::utils::misc::format_size", 1)]
        );
    }

    #[test]
    fn a_type_qualifier_matches_only_that_types_members() {
        let mut symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            symbol_in(
                Language::Rust,
                "file",
                "File",
                "core::lib::File",
                SymbolKind::Class,
            ),
            symbol_in(
                Language::Rust,
                "scope-kind",
                "ScopeKind",
                "core::lib::ScopeKind",
                SymbolKind::Class,
            ),
            symbol_in(
                Language::Rust,
                "store",
                "Store",
                "db::lib::Store",
                SymbolKind::Class,
            ),
            symbol_in(
                Language::Rust,
                "cache",
                "Cache",
                "db::lib::Cache",
                SymbolKind::Class,
            ),
            symbol_in(
                Language::Rust,
                "open",
                "open",
                "db::lib::open",
                SymbolKind::Method,
            ),
        ];
        symbols
            .iter_mut()
            .find(|symbol| symbol.id.0 == "open")
            .expect("the method")
            .parent_symbol_id = Some(SymbolId::new("store"));
        let text = "let kind = ScopeKind::File;\nlet cache = Cache::open(dir);\nlet store = Store::open(dir);";
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![
                reference("core::lib::ScopeKind", 1),
                reference("db::lib::Cache", 2),
                reference("db::lib::Store", 3),
                call("db::lib::open", 3),
            ]
        );
    }

    #[test]
    fn a_let_binds_its_name_only_after_its_value() {
        let symbols = unique_functions(Language::Rust, &["config"]);
        let text = "let config = config(dir);\nuse_it(config);";
        let report = resolve_text(Language::Rust, text, &symbols);
        assert_eq!(line_targets(&report), vec![call("util::config", 1)]);

        let symbols = unique_functions(Language::Python, &["config"]);
        let text = "config = config(path)\nuse_it(config)";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(line_targets(&report), vec![call("util::config", 1)]);
    }

    #[test]
    fn a_named_escape_in_an_f_string_is_text() {
        let symbols = unique_functions(Language::Python, &["BULLET", "item"]);
        let text = "line = f\"\\N{BULLET} {item}\"";
        let report = resolve_text(Language::Python, text, &symbols);
        assert_eq!(line_targets(&report), vec![reference("util::item", 1)]);
    }

    #[test]
    fn go_composite_literal_keys_and_short_declarations_are_not_matched_by_name_alone() {
        let symbols = vec![
            symbol_in(
                Language::Go,
                "caller",
                "main",
                "app::main",
                SymbolKind::Function,
            ),
            symbol_in(
                Language::Go,
                "ports",
                "ports",
                "topology::ports",
                SymbolKind::Function,
            ),
            symbol_in(
                Language::Go,
                "reason",
                "Reason",
                "gate::Reason",
                SymbolKind::Class,
            ),
            symbol_in(
                Language::Go,
                "limit",
                "parseLimit",
                "api::parseLimit",
                SymbolKind::Function,
            ),
            symbol_in(Language::Go, "kind", "Kind", "api::Kind", SymbolKind::Class),
        ];
        let text = "ports := []string{\"8500\"}\nuse(ports)\nresp := Response{\n    Reason: \"x\",\n}\nif err := parseLimit(req); err != nil {\n}\nswitch k {\ncase Kind:\n}";
        let report = resolve_text(Language::Go, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![call("api::parseLimit", 6), reference("api::Kind", 9)]
        );
    }

    #[test]
    fn a_wide_character_before_a_member_path_quote_or_brace_reads_without_panicking() {
        // A character wider than a byte just before a word used to leave a slice mid-character.
        let text = "x = a·.join(b)\ny = —.lower()\nz = ·f'{v}'\nw = Ω::new()\nlet s = é·Foo { a, b };\nv := ·pkg.Call()";
        for language in [
            Language::Rust,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Java,
            Language::Go,
            Language::Markdown,
        ] {
            let _ = token_uses(text, &language);
        }
        let symbols = unique_functions(Language::Python, &["join", "value"]);
        let report = resolve_text(Language::Python, "z = ·f'{value}'", &symbols);
        assert_eq!(line_targets(&report), vec![reference("util::value", 1)]);
    }
}
