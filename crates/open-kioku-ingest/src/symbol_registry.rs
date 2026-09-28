use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, ImportResolution, Language, QualityNote, QualityNoteKind, ResolutionStatus,
    Scope, ScopeId, ScopeKind, StringInterner, Symbol, SymbolId, SymbolKind,
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
    /// A member's receiver when it is a plain name, outside an import line: `structs` of
    /// `structs.NewCheckID(..)`, `Constants` of `Constants.ACCESS_KEY`.
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
}

struct RustFileScopes<'a> {
    path: &'a Path,
    scopes: Vec<&'a Scope>,
}

impl<'a> RegistryScopeModel<'a> {
    pub fn new(
        files: &'a [File],
        repository: &'a SemanticRepository,
        symbols: &'a SymbolIndex,
        scopes: &'a ScopeIndex,
        bindings: &'a BindingIndex,
        inheritance: &'a InheritanceIndex,
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
        Self {
            repository,
            symbols,
            scopes,
            bindings,
            inheritance,
            rust_files,
            module_names,
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
        let admits = |symbol: &Symbol| scope.admits(symbol);
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
                // member lacks: Go's `structs.NewCheckID`, Java's `Constants.ACCESS_KEY`.
                // A lowercase Java receiver is a variable, whose name may match a package's.
                let receiver = token_use.receiver.as_deref().filter(|receiver| {
                    chunk.language != Language::Java || receiver.starts_with(char::is_uppercase)
                });
                let Some(receiver) = receiver else {
                    return unresolved(TokenRole::Member.withheld_reason().unwrap_or_default());
                };
                let owned =
                    |symbol: &Symbol| admits(symbol) && segment_locates(self, receiver, symbol);
                return self
                    .resolve_unique_project_name(chunk, token, &owned)
                    .unwrap_or_else(|| {
                        unresolved(TokenRole::Member.withheld_reason().unwrap_or_default())
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
                })
            })
    }

    /// Where the path or import spelling a token says its target is.
    fn origin<'t>(
        &self,
        chunk: &CodeChunk,
        token_use: &'t TokenUse,
        model: Option<&RegistryScopeModel<'_>>,
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
        let imported_from_outside = model.is_some_and(|model| {
            let bindings = model.file_imports(&chunk.file_id, token);
            !bindings.is_empty()
                && bindings.iter().all(|binding| {
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
                })
        });
        if imported_from_outside {
            Origin::Outside
        } else {
            Origin::Anywhere
        }
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
        scoped_resolution("direct-import", candidates, admits, Confidence::High, false)
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
        scoped_resolution("same-file", candidates, admits, Confidence::High, false)
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
        scoped_resolution(
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
        scoped_resolution(
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
        scoped_resolution(
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
        scoped_resolution(
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
    let registry = SymbolRegistry::new(symbols, import_resolutions);
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

/// `resolution_from_candidates` over the candidates `admits` keeps, when it keeps all or none.
///
/// Ruling some candidates out does not show that the name means the rest: a `mod tests` helper
/// out of reach of a `let path = dir.path()` says nothing about the `path` method that remains.
/// A strategy that matched a ruled-out item beside others stays ambiguous, as it was. With no
/// survivor the strategy matched nothing and the next one runs.
fn scoped_resolution(
    strategy: &'static str,
    candidates: Vec<Symbol>,
    admits: &dyn Fn(&Symbol) -> bool,
    confidence: Confidence,
    speculative: bool,
) -> Option<Resolution> {
    let (kept, dropped): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(|symbol| admits(symbol));
    if kept.is_empty() || dropped.is_empty() {
        return resolution_from_candidates(strategy, kept, confidence, speculative);
    }
    resolution_from_candidates(
        strategy,
        kept.into_iter().chain(dropped).collect(),
        confidence,
        speculative,
    )
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
        let mut repository = SemanticRepository::new();
        for (local, source) in imports {
            repository
                .imports
                .by_file_local_name
                .entry((FileId::new("entry"), local.to_string()))
                .or_default()
                .push(ImportBinding {
                    file_id: FileId::new("entry"),
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
            &[],
            &repository,
            &symbol_index,
            &scopes,
            &bindings,
            &inheritance,
        );
        resolve_symbol_edges(
            &[chunk_in(language, text)],
            symbols,
            &[],
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

    #[test]
    fn a_java_static_import_of_a_repository_member_is_not_from_outside() {
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
        let text = "String key = ACCESS_KEY;";
        let report = resolve_with_imports_in(
            Language::Java,
            text,
            &symbols,
            &[("ACCESS_KEY", "static org.example.Constants.ACCESS_KEY")],
        );
        assert_eq!(
            line_targets(&report),
            vec![reference(
                "src::main::java::org::example::Constants::ACCESS_KEY",
                1
            )]
        );

        // A static import from a library still leads outside the repository.
        let report = resolve_with_imports_in(
            Language::Java,
            text,
            &symbols,
            &[("ACCESS_KEY", "static com.vendor.Keys.ACCESS_KEY")],
        );
        assert_eq!(line_targets(&report), vec![]);
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
                "new-check",
                "NewCheckID",
                "agent::structs::checks::NewCheckID",
                SymbolKind::Function,
            ),
        ];
        let text = "id := structs.NewCheckID(name)\nother := check.NewCheckID(name)";
        let report = resolve_text(Language::Go, text, &symbols);
        assert_eq!(
            line_targets(&report),
            vec![call("agent::structs::checks::NewCheckID", 1)]
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
