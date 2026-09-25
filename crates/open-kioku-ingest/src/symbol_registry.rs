use open_kioku_core::{
    identity, AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, ImportResolution, Language, QualityNote, QualityNoteKind, ResolutionStatus,
    Scope, ScopeId, StringInterner, Symbol, SymbolId, SymbolKind,
};
use open_kioku_resolution::{
    context::rust_rules_out_same_file_item, BindingIndex, InheritanceIndex, ResolutionContext,
    ScopeIndex, SymbolIndex,
};
use open_kioku_semantic_model::SemanticRepository;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
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
    /// Not the tail of a `path::` or the member of a `receiver.`: only a bare name is looked up
    /// in the scopes around its use.
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
        Self {
            repository,
            symbols,
            scopes,
            bindings,
            inheritance,
            rust_files,
        }
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
        match site {
            Some((ctx, scope_id)) => !rust_rules_out_same_file_item(ctx, scope_id, token, symbol),
            None => true,
        }
    }
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
        if let Some(resolution) = self.resolve_unique_project_name(token, &admits) {
            return resolution;
        }
        if let Some(resolution) =
            self.resolve_suffix_with_import_reachability(chunk, token, &admits)
        {
            return resolution;
        }
        self.resolve_fuzzy(token, &admits)
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
            .filter(|symbol| symbol_matches_token(symbol, token) && admits(symbol))
            .cloned()
            .collect::<Vec<_>>();
        resolution_from_candidates("same-file", candidates, Confidence::High, false)
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
        scoped_resolution("same-module", candidates, admits, Confidence::Medium, false)
    }

    fn resolve_unique_project_name(
        &self,
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
            symbols,
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
            candidates,
            admits,
            Confidence::Low,
            true,
        )
    }

    fn resolve_fuzzy(&self, token: &str, admits: &dyn Fn(&Symbol) -> bool) -> Option<Resolution> {
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
        scoped_resolution("fuzzy-fallback", candidates, admits, Confidence::Low, true)
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
    let mut seen = HashSet::new();

    for token_use in token_uses(&chunk.text)
        .into_iter()
        .take(MAX_TOKENS_PER_CHUNK)
    {
        if chunk
            .symbol_id
            .as_ref()
            .and_then(|id| registry.by_id.get(id))
            .is_some_and(|symbol| symbol.name == token_use.token)
        {
            continue;
        }
        let scope = ScopeFilter::new(scope_model, chunk, &token_use);
        let resolution = registry.resolve(chunk, &token_use.token, &scope);
        let resolved_id = resolution
            .symbol
            .as_ref()
            .map(|symbol| symbol.id.0.as_str())
            .unwrap_or("<unresolved>");
        let dedup_key = (
            token_use.token.clone(),
            token_use.line,
            resolved_id.to_owned(),
        );
        if !seen.insert(dedup_key) {
            continue;
        }

        if let Some(note) = quality_note(&token_use.token, &resolution) {
            notes.push(note);
        }
        if let Some(fact) =
            fact_for_resolution(chunk, &token_use, &resolution, scip_available, interner)
        {
            facts.push(fact);
        } else if !unresolved.contains(&token_use.token) {
            unresolved.push(token_use.token);
        }
    }
    ChunkResolution {
        facts,
        notes,
        unresolved,
    }
}

/// `resolution_from_candidates` over the candidates `admits` keeps.
///
/// Only the same-file match is decided by the scoping rule, so only there may dropping an item
/// leave a unique winner. Elsewhere the dropped item was a match the strategy's own rule found:
/// a name that is not unique in the project, or an import reaching two items, stays ambiguous
/// rather than turning the survivor into a new edge. With no survivor the strategy matched
/// nothing and the next one runs.
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

fn token_uses(text: &str) -> Vec<TokenUse> {
    let mut uses = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let mut current = String::new();
        let mut token_end = 0usize;
        for (idx, ch) in line.char_indices() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                current.push(ch);
                token_end = idx + ch.len_utf8();
            } else if !current.is_empty() {
                push_token_use(&mut uses, &current, line, token_end, line_index);
                current.clear();
            }
        }
        if !current.is_empty() {
            push_token_use(&mut uses, &current, line, token_end, line_index);
        }
    }
    uses
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
    let bare = !(before.ends_with("::") || (before.ends_with('.') && !before.ends_with("..")));
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
