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
    by_name_suffix: HashMap<String, Vec<SymbolId>>,
    qualified_name_normalized: HashMap<String, String>,
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
            by_name_suffix: HashMap::new(),
            qualified_name_normalized: HashMap::new(),
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
            registry
                .by_module
                .entry(module_name(&symbol.qualified_name))
                .or_default()
                .push(symbol.id.clone());
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

    fn resolve(&self, chunk: &CodeChunk, token: &str, scope: &ScopeFilter<'_, '_>) -> Resolution {
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
        if let Some(resolution) = self.resolve_unique_project_name(chunk, token, &admits) {
            return resolution;
        }
        if let Some(resolution) =
            self.resolve_suffix_with_import_reachability(chunk, token, &admits)
        {
            return resolution;
        }
        self.resolve_fuzzy(chunk, token, &admits)
            .unwrap_or_else(|| Resolution {
                symbol: None,
                strategy: "unresolved",
                candidates: 0,
                confidence: Confidence::Low,
                ambiguity_reason: Some("no registry candidate matched".into()),
                speculative: true,
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
                candidates.extend(
                    self.by_file
                        .get(file_id)
                        .into_iter()
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
            let resolution = registry.resolve(chunk, &token_use.token, &scope);
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
    for (line_index, line) in text.lines().enumerate() {
        for span in lexer.code_spans(line) {
            let mut token: Option<(usize, usize)> = None;
            for (offset, ch) in line[span.clone()].char_indices() {
                let idx = span.start + offset;
                if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                    let start = token.map_or(idx, |(start, _)| start);
                    token = Some((start, idx + ch.len_utf8()));
                } else if let Some((start, end)) = token.take() {
                    push_code_token(&mut uses, language, line, start..end, line_index);
                }
            }
            if let Some((start, end)) = token {
                push_code_token(&mut uses, language, line, start..end, line_index);
            }
        }
    }
    uses
}

/// A code token, unless it is a string prefix such as Rust's `br` or Python's `rb`, which reads
/// as a name beside the literal it opens.
fn push_code_token(
    uses: &mut Vec<TokenUse>,
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
    if !opens_literal {
        push_token_use(uses, text, line, token.end, line_index);
    }
}

/// Where a chunk's code is, as opposed to its comments and string literals, whose words name
/// nothing: `// the clock's now` and `"now"` are not uses of `now` (#563).
///
/// A lexical pass, not a parse. It knows each language's comment and literal delimiters and
/// carries block comments, multi-line literals and template interpolations across lines. A chunk
/// is read from its first line, so one that starts inside a literal is read as code there. A
/// language it has no rules for (JSON, Markdown, plain text) is read as code throughout, as
/// before. Regular-expression literals and interpolated Python and Rust strings are not
/// modeled: a regex reads as code, and an f-string's or format string's names read as literal.
struct CodeLexer {
    syntax: LexicalSyntax,
    /// Innermost last. Empty is code outside any template interpolation.
    stack: Vec<LexState>,
}

#[derive(Clone, Copy)]
struct LexicalSyntax {
    line_comment: Option<&'static str>,
    block_comments: bool,
    nested_block_comments: bool,
    /// `'` opens a literal; with `lifetimes`, only when a character literal closes it.
    single_quote_strings: bool,
    lifetimes: bool,
    raw_strings: bool,
    triple_quotes: bool,
    backtick: Backtick,
    /// A `"` or `'` literal may continue on the next line.
    multiline_strings: bool,
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
}

enum LexStep {
    Advance(usize),
    Push(LexState, usize),
    Pop(usize),
    LineComment,
}

impl CodeLexer {
    fn new(language: &Language) -> Self {
        let c_like = LexicalSyntax {
            line_comment: Some("//"),
            block_comments: true,
            nested_block_comments: false,
            single_quote_strings: true,
            lifetimes: false,
            raw_strings: false,
            triple_quotes: false,
            backtick: Backtick::Code,
            multiline_strings: false,
        };
        let plain = LexicalSyntax {
            line_comment: None,
            block_comments: false,
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
                ..c_like
            },
            // A double-quoted SQL name is an identifier, not a literal.
            Language::Sql => LexicalSyntax {
                line_comment: Some("--"),
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
    fn code_spans(&mut self, line: &str) -> Vec<Range<usize>> {
        let mut spans = Vec::new();
        let mut code_start = self.in_code().then_some(0);
        let mut idx = 0;
        while idx < line.len() {
            let step = match self.stack.last().copied() {
                None | Some(LexState::Interpolation(_)) => self.code_step(line, idx),
                Some(state) => self.literal_step(state, &line.as_bytes()[idx..]),
            };
            match step {
                LexStep::Advance(len) => idx += len,
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
        // does not hide the rest of the chunk.
        if !self.syntax.multiline_strings {
            while let Some(LexState::Str { triple: false, .. }) = self.stack.last() {
                self.stack.pop();
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
                if quote == b'\'' && !syntax.single_quote_strings {
                    return LexStep::Advance(1);
                }
                if quote == b'\'' && syntax.lifetimes && !opens_char_literal(&line[idx + 1..]) {
                    return LexStep::Advance(1);
                }
                let triple = syntax.triple_quotes && rest.starts_with(&[quote; 3]);
                LexStep::Push(LexState::Str { quote, triple }, if triple { 3 } else { 1 })
            }
            b'`' => match syntax.backtick {
                Backtick::Template => LexStep::Push(LexState::Template, 1),
                Backtick::Raw => LexStep::Push(LexState::GoRaw, 1),
                Backtick::Code => LexStep::Advance(1),
            },
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
            // Code states are stepped by `code_step`.
            LexState::Interpolation(_) => LexStep::Advance(1),
        }
    }

    fn in_code(&self) -> bool {
        matches!(self.stack.last(), None | Some(LexState::Interpolation(_)))
    }

    fn replace_top(&mut self, state: LexState) {
        if let Some(top) = self.stack.last_mut() {
            *top = state;
        }
    }
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
    token: &str,
    line: &str,
    token_end: usize,
    line_index: usize,
) {
    if is_keyword_or_literal(token) || token.len() < 2 {
        return;
    }
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
    });
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
        // The one `now` in the repository is JavaScript: a Rust `Utc::now()` is not a call of it.
        let javascript_now = with_language(
            symbol("js-now", "site", "now", "site::now", SymbolKind::Function),
            Language::JavaScript,
        );
        let mut symbols = vec![
            rust_symbol("caller", "entry", "main", "app::main"),
            javascript_now,
        ];
        let rust_call = chunk_in(Language::Rust, "let at = Utc::now();");
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
}
