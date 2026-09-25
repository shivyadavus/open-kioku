use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use crate::inheritance::InheritanceIndex;
use open_kioku_core::{
    Confidence, FileId, Language, ModuleId, Scope, ScopeId, ScopeKind, Symbol, SymbolId,
};
use open_kioku_languages::semantics::LanguageSemantics;
use open_kioku_semantic_model::{ImportBinding, SemanticRepository, GLOB_IMPORT_LOCAL_NAME};

/// Scope id the import registry gives a binding whose site recorded no scope.
const UNSCOPED_IMPORT: &str = "global";

/// What the imports in scope at a use site say about one name.
#[derive(Debug)]
pub(crate) enum ScopedImport<'r> {
    /// No import in scope names it.
    NotImported,
    /// The nearest scope that imports the name binds it, and every binding there is resolved.
    Resolved(Vec<&'r ImportBinding>),
    /// The nearest import of the name is unresolved, or a glob import in a nearer scope may supply
    /// the name instead.
    Unresolved,
}

/// Looks `name` up through the scopes enclosing `scope_id`, nearest first. An import shadows the
/// same name imported further out, and an explicit import shadows a glob in its own scope. A Rust
/// `mod` block does not see the imports of the module around it, so the walk stops at one:
/// `use crate::auth::f;` at file level does not reach `mod tests { use crate::fakes::*; }`.
/// A Python `if`, `try`, `with` or loop body is not a namespace, so an import made in one binds in
/// the enclosing function, class or module; and a function body does not see its class body's
/// names, so the walk skips class scopes once it has left a function.
/// Without a scope index or a use-site scope, every import of the name in the file is one set.
pub(crate) fn scoped_import<'r>(
    repository: &'r SemanticRepository,
    scopes: Option<&ScopeIndex>,
    language: &Language,
    file_id: &FileId,
    scope_id: Option<&ScopeId>,
    name: &str,
    is_resolved: impl Fn(&ImportBinding) -> bool,
) -> ScopedImport<'r> {
    let named = file_imports(repository, file_id, name)
        .iter()
        .filter(|binding| !binding.is_glob)
        .collect::<Vec<_>>();
    let globs = file_imports(repository, file_id, GLOB_IMPORT_LOCAL_NAME);
    let settle = |bindings: Vec<&'r ImportBinding>| {
        if bindings.iter().all(|binding| is_resolved(binding)) {
            ScopedImport::Resolved(bindings)
        } else {
            ScopedImport::Unresolved
        }
    };

    let (Some(scopes), Some(scope_id)) = (scopes, scope_id) else {
        return if !named.is_empty() {
            settle(named)
        } else if !globs.is_empty() {
            ScopedImport::Unresolved
        } else {
            ScopedImport::NotImported
        };
    };

    let python = *language == Language::Python;
    let mut left_function = false;
    let mut current = Some(scope_id);
    let mut steps = 0usize;
    while let Some(id) = current {
        let scope = scopes.get(id);
        let visible = !(python
            && left_function
            && nearest_non_block_scope(scopes, id)
                .is_some_and(|owner| owner.kind == ScopeKind::Class));
        let here = named
            .iter()
            .copied()
            .filter(|binding| visible && binds_at(scopes, language, &binding.scope_id, id))
            .collect::<Vec<_>>();
        if !here.is_empty() {
            return settle(here);
        }
        let globs_here = globs
            .iter()
            .filter(|binding| visible && binds_at(scopes, language, &binding.scope_id, id))
            .collect::<Vec<_>>();
        if !globs_here.is_empty() {
            // `use super::*;` names the parent module's namespace, the parent's own imports
            // included, so the lookup continues there. Any other glob may supply the name.
            let parent = if *language == Language::Rust {
                rust_super_glob_target(scopes, id, &globs_here)
            } else {
                None
            };
            let Some(parent) = parent else {
                return ScopedImport::Unresolved;
            };
            steps += 1;
            if steps > scopes.scopes.len() {
                break;
            }
            current = Some(parent);
            continue;
        }
        if *language == Language::Rust && scope.is_some_and(|scope| scope.kind == ScopeKind::Module)
        {
            return ScopedImport::NotImported;
        }
        if python
            && scope.is_some_and(|scope| {
                matches!(
                    scope.kind,
                    ScopeKind::Function | ScopeKind::Method | ScopeKind::Closure
                )
            })
        {
            left_function = true;
        }
        steps += 1;
        if steps > scopes.scopes.len() {
            break;
        }
        current = scope.and_then(|scope| scope.parent_id.as_ref());
    }

    let unscoped = named
        .iter()
        .copied()
        .filter(|binding| binding.scope_id.0 == UNSCOPED_IMPORT)
        .collect::<Vec<_>>();
    if !unscoped.is_empty() {
        settle(unscoped)
    } else if globs
        .iter()
        .any(|binding| binding.scope_id.0 == UNSCOPED_IMPORT)
    {
        ScopedImport::Unresolved
    } else {
        ScopedImport::NotImported
    }
}

/// Items of this file named `name` in the nearest scope around `scope_id` that declares one
/// `accept` admits, or `None` when lexical lookup reaches none.
///
/// Outside Rust the walk follows every enclosing scope. A Rust item is visible by its bare name
/// only inside the module that declares it, so the walk stops where the name could come from
/// somewhere other than an enclosing item and leaves that case to the import rule: at a scope
/// that imports the name, at a scope with a glob import, and at a `mod` block. Two imports name a
/// module of this same file and continue there instead: `use super::name;`, `use self::name;` or
/// `use super::super::name;` looks the imported name up in the module it names, and a `mod` block
/// whose only globs are the same `use super::*` path continues in that module, as
/// [`scoped_import`] does. Beside another glob, `use super::*` still reaches an item the parent
/// module declares itself: were the other glob to supply the name too, rustc would reject the use
/// as ambiguous. Associated items of an `impl` or `trait` are never in lexical scope.
pub(crate) fn nearest_lexical_items(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    name: &str,
    accept: impl Fn(&Symbol) -> bool,
) -> Option<Vec<SymbolId>> {
    let rust = ctx.language == Language::Rust;
    let mut name = name;
    let mut current = Some(scope_id);
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = current {
        if !visited.insert((id, name)) {
            break;
        }
        let scope = ctx.scopes.get(id);
        let associated = rust
            && scope.is_some_and(|scope| matches!(scope.kind, ScopeKind::Class | ScopeKind::Trait));
        if !associated {
            let items = declared_items(ctx, id, name, &accept);
            if !items.is_empty() {
                return Some(items);
            }
        }
        current = if rust {
            match rust_lexical_next(ctx, id, name) {
                RustLexicalStep::Continue(next, imported) => {
                    name = imported;
                    Some(next)
                }
                RustLexicalStep::DeclaredIn(module) => {
                    let items = declared_items(ctx, module, name, &accept);
                    return (!items.is_empty()).then_some(items);
                }
                RustLexicalStep::Stop => None,
            }
        } else {
            scope.and_then(|scope| scope.parent_id.as_ref())
        };
    }
    None
}

/// Whether Rust scoping rules out `candidate`, an item of this file, as the target of `name` used
/// at `scope_id`.
///
/// The nearest explicit import of the name in the use site's module decides when it has one: it
/// rules the item out when it names another path, and keeps it when it names this item or a
/// path of this crate the index could not place. Without one, the item is ruled out when its
/// module is neither the use site's nor one the use site reaches through `use super::*` globs.
/// Unknown scopes rule nothing out.
pub(crate) fn rust_rules_out_same_file_item(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    name: &str,
    candidate: &Symbol,
) -> bool {
    let named = file_imports(ctx.repository, ctx.file_id, name)
        .iter()
        .filter(|binding| !binding.is_glob)
        .collect::<Vec<_>>();
    let mut current = ctx.scopes.get(scope_id);
    for _ in 0..=ctx.scopes.scopes.len() {
        let Some(scope) = current else {
            break;
        };
        let here = named
            .iter()
            .copied()
            .filter(|binding| binding.scope_id == scope.id)
            .collect::<Vec<_>>();
        if !here.is_empty() {
            return here
                .iter()
                .any(|binding| rust_import_names_another_item(ctx.scopes, binding, candidate));
        }
        if matches!(scope.kind, ScopeKind::Module | ScopeKind::File) {
            break;
        }
        current = scope
            .parent_id
            .as_ref()
            .and_then(|parent| ctx.scopes.get(parent));
    }
    let Some(item_module) = candidate
        .scope_id
        .as_ref()
        .and_then(|item_scope| enclosing_module_scope(ctx.scopes, item_scope))
    else {
        return false;
    };
    let Some(use_module) = enclosing_module_scope(ctx.scopes, scope_id) else {
        return false;
    };
    !rust_module_reaches_through_super_globs(ctx, use_module, &item_module.id)
}

/// Whether `binding`, an explicit import of the candidate's name, names something other than
/// `candidate`. An in-crate path the index could not bind may be the candidate itself, so it
/// does not rule the candidate out.
fn rust_import_names_another_item(
    scopes: &ScopeIndex,
    binding: &ImportBinding,
    candidate: &Symbol,
) -> bool {
    if let Some(target) = &binding.target_symbol {
        return *target != candidate.id;
    }
    if let Some(target_file) = &binding.target_file {
        return *target_file != candidate.file_id;
    }
    let source = binding.source_module.as_str();
    if source.starts_with("self::") || source.starts_with("super::") {
        let item_module = candidate
            .scope_id
            .as_ref()
            .and_then(|item_scope| enclosing_module_scope(scopes, item_scope));
        return match (
            rust_relative_item_target(scopes, &binding.scope_id, &[binding]),
            item_module,
        ) {
            (Some((module, item)), Some(item_module)) => {
                *module != item_module.id || item != candidate.name
            }
            // A relative path climbing out of the file names another module.
            _ => true,
        };
    }
    !source.starts_with("crate::")
}

/// Whether `from` is `target`, or reaches it through a chain of `use super::*` globs, each
/// declared by the module it leaves.
fn rust_module_reaches_through_super_globs(
    ctx: &ResolutionContext<'_>,
    from: &Scope,
    target: &ScopeId,
) -> bool {
    let globs = file_imports(ctx.repository, ctx.file_id, GLOB_IMPORT_LOCAL_NAME);
    let mut pending = vec![&from.id];
    let mut seen = std::collections::HashSet::new();
    while let Some(module) = pending.pop() {
        if module == target {
            return true;
        }
        if !seen.insert(module) {
            continue;
        }
        for glob in globs.iter().filter(|glob| &glob.scope_id == module) {
            if rust_super_glob_depth(&glob.source_module).is_some() {
                if let Some(parent) = rust_super_glob_target(ctx.scopes, module, &[glob]) {
                    pending.push(parent);
                }
            }
        }
    }
    false
}

/// Items of this file named `name` that `scope_id` itself declares and `accept` admits.
fn declared_items(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    name: &str,
    accept: &impl Fn(&Symbol) -> bool,
) -> Vec<SymbolId> {
    let mut items = ctx
        .symbols
        .lookup_file_scope_name(ctx.file_id, scope_id, name)
        .iter()
        .filter(|item| ctx.symbols.get(item).is_some_and(accept))
        .cloned()
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.0.cmp(&right.0));
    items.dedup();
    items
}

/// What a Rust lexical lookup does after a scope declares no matching item.
enum RustLexicalStep<'s, 'n> {
    /// Look `name` (possibly renamed by a `use super::name as alias;`) up in the scope.
    Continue(&'s ScopeId, &'n str),
    /// Only an item this module declares itself can be the target; nothing it imports is.
    DeclaredIn(&'s ScopeId),
    /// The item cannot be proven to come from a scope of this file.
    Stop,
}

/// Where a Rust lexical lookup of `name` continues after `scope_id` declares no such item.
fn rust_lexical_next<'s, 'n>(
    ctx: &ResolutionContext<'s>,
    scope_id: &ScopeId,
    name: &'n str,
) -> RustLexicalStep<'s, 'n>
where
    's: 'n,
{
    let binds_here = |binding: &&ImportBinding| &binding.scope_id == scope_id;
    let named_here = file_imports(ctx.repository, ctx.file_id, name)
        .iter()
        .filter(|binding| !binding.is_glob)
        .filter(binds_here)
        .collect::<Vec<_>>();
    if !named_here.is_empty() {
        return match rust_relative_item_target(ctx.scopes, scope_id, &named_here) {
            Some((next, imported)) => RustLexicalStep::Continue(next, imported),
            None => RustLexicalStep::Stop,
        };
    }
    let Some(scope) = ctx.scopes.get(scope_id) else {
        return RustLexicalStep::Stop;
    };
    let globs_here = file_imports(ctx.repository, ctx.file_id, GLOB_IMPORT_LOCAL_NAME)
        .iter()
        .filter(binds_here)
        .collect::<Vec<_>>();
    if !globs_here.is_empty() {
        // A glob in a block may shadow an outer item, so a name the block does not declare stays
        // open there. A module's globs cannot shadow one another: two that supply the same name
        // make its use ambiguous, so `use super::*` beside other globs is exact for an item the
        // parent declares, though not for one the parent only imports, which another glob may
        // name as well.
        if scope.kind != ScopeKind::Module {
            return RustLexicalStep::Stop;
        }
        if let Some(parent) = rust_super_glob_target(ctx.scopes, scope_id, &globs_here) {
            return RustLexicalStep::Continue(parent, name);
        }
        let super_globs = globs_here
            .iter()
            .copied()
            .filter(|glob| rust_super_glob_depth(&glob.source_module).is_some())
            .collect::<Vec<_>>();
        return match rust_super_glob_target(ctx.scopes, scope_id, &super_globs) {
            Some(parent) => RustLexicalStep::DeclaredIn(parent),
            None => RustLexicalStep::Stop,
        };
    }
    if scope.kind == ScopeKind::Module {
        return RustLexicalStep::Stop;
    }
    match scope.parent_id.as_ref() {
        Some(parent) => RustLexicalStep::Continue(parent, name),
        None => RustLexicalStep::Stop,
    }
}

/// The module scope and item name that the imports at `scope_id` name, when every one of them is
/// the same `self::item`, `super::item` or `super::super::item` path and the module is in this
/// file.
fn rust_relative_item_target<'s>(
    scopes: &'s ScopeIndex,
    scope_id: &ScopeId,
    imports: &[&'s ImportBinding],
) -> Option<(&'s ScopeId, &'s str)> {
    let (depth, item) = rust_relative_item_path(&imports.first()?.source_module)?;
    if imports
        .iter()
        .any(|import| rust_relative_item_path(&import.source_module) != Some((depth, item)))
    {
        return None;
    }
    let mut module = enclosing_module_scope(scopes, scope_id)?;
    for _ in 0..depth {
        if module.kind != ScopeKind::Module {
            return None;
        }
        module = enclosing_module_scope(scopes, module.parent_id.as_ref()?)?;
    }
    Some((&module.id, item))
}

/// `self::item` is `(0, "item")`, `super::item` is `(1, "item")` and `super::super::item` is
/// `(2, "item")`; any other path, a glob included, is `None`.
fn rust_relative_item_path(source: &str) -> Option<(usize, &str)> {
    let (path, item) = source.rsplit_once("::")?;
    if item == "*" || item.is_empty() {
        return None;
    }
    if path == "self" {
        return Some((0, item));
    }
    let mut depth = 0usize;
    for segment in path.split("::") {
        if segment != "super" {
            return None;
        }
        depth += 1;
    }
    Some((depth, item))
}

/// Whether an import recorded at `binding_scope` binds its names at `level`. In Python the import
/// also binds in each block enclosing it, up to the nearest function, class or module.
fn binds_at<'s>(
    scopes: &'s ScopeIndex,
    language: &Language,
    binding_scope: &'s ScopeId,
    level: &ScopeId,
) -> bool {
    if *language != Language::Python {
        return binding_scope == level;
    }
    let mut current = binding_scope;
    for _ in 0..=scopes.scopes.len() {
        if current == level {
            return true;
        }
        match scopes.get(current) {
            Some(scope) if scope.kind == ScopeKind::Block => match scope.parent_id.as_ref() {
                Some(parent) => current = parent,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}

/// `scope_id` itself, or the nearest scope above it, that is not a block.
fn nearest_non_block_scope<'s>(scopes: &'s ScopeIndex, scope_id: &ScopeId) -> Option<&'s Scope> {
    let mut current = scopes.get(scope_id);
    for _ in 0..=scopes.scopes.len() {
        let scope = current?;
        if scope.kind != ScopeKind::Block {
            return Some(scope);
        }
        current = scope
            .parent_id
            .as_ref()
            .and_then(|parent| scopes.get(parent));
    }
    None
}

/// The module scope that the globs at `scope_id` name, when every one of them is the same
/// `super::*` or `super::super::*` path. `None` when another glob may supply a name, or when the
/// path climbs past the file's own module, whose parent the scopes of one file cannot show.
fn rust_super_glob_target<'s>(
    scopes: &'s ScopeIndex,
    scope_id: &ScopeId,
    globs: &[&ImportBinding],
) -> Option<&'s ScopeId> {
    let depth = rust_super_glob_depth(&globs.first()?.source_module)?;
    if globs
        .iter()
        .any(|glob| rust_super_glob_depth(&glob.source_module) != Some(depth))
    {
        return None;
    }
    let mut module = enclosing_module_scope(scopes, scope_id)?;
    for _ in 0..depth {
        if module.kind != ScopeKind::Module {
            return None;
        }
        module = enclosing_module_scope(scopes, module.parent_id.as_ref()?)?;
    }
    Some(&module.id)
}

/// `super::*` is 1 and `super::super::*` is 2; any other path is `None`.
fn rust_super_glob_depth(source: &str) -> Option<usize> {
    let path = source.strip_suffix("::*")?;
    let segments = path.split("::").collect::<Vec<_>>();
    (!segments.is_empty() && segments.iter().all(|segment| *segment == "super"))
        .then_some(segments.len())
}

/// The nearest module or file scope at or above `scope_id`.
fn enclosing_module_scope<'s>(scopes: &'s ScopeIndex, scope_id: &ScopeId) -> Option<&'s Scope> {
    let mut current = scopes.get(scope_id);
    for _ in 0..=scopes.scopes.len() {
        let scope = current?;
        if matches!(scope.kind, ScopeKind::Module | ScopeKind::File) {
            return Some(scope);
        }
        current = scope
            .parent_id
            .as_ref()
            .and_then(|parent| scopes.get(parent));
    }
    None
}

fn file_imports<'r>(
    repository: &'r SemanticRepository,
    file_id: &FileId,
    local_name: &str,
) -> &'r [ImportBinding] {
    repository
        .imports
        .by_file_local_name
        .get(&(file_id.clone(), local_name.to_string()))
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedReason {
    NoCandidate,
    AmbiguousName,
    UnknownReceiverType,
    AmbiguousReceiverType,
    UnresolvedImport,
    VisibilityViolation,
    IncompatibleKind,
    UnsupportedDynamicDispatch,
    ExternalDependency,
}

#[derive(Debug, Clone)]
pub enum ResolutionResult {
    Resolved {
        target: SymbolId,
        confidence: Confidence,
        evidence: Vec<ResolutionEvidence>,
    },
    Ambiguous {
        candidates: Vec<SymbolId>,
        reason: String,
        evidence: Vec<ResolutionEvidence>,
    },
    External {
        package: String,
    },
    Unresolved {
        reason: UnresolvedReason,
        evidence: Vec<ResolutionEvidence>,
    },
}

pub struct ResolutionContext<'a> {
    pub file_id: &'a FileId,
    pub file_path: &'a std::path::Path,
    pub module_id: Option<&'a ModuleId>,
    pub language: Language,
    pub repository: &'a SemanticRepository,
    pub symbols: &'a SymbolIndex,
    pub scopes: &'a ScopeIndex,
    pub bindings: &'a BindingIndex,
    pub inheritance: &'a InheritanceIndex,
    pub semantics: &'static dyn LanguageSemantics,
}

impl<'a> ResolutionContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file_id: &'a FileId,
        file_path: &'a std::path::Path,
        module_id: Option<&'a ModuleId>,
        language: Language,
        repository: &'a SemanticRepository,
        symbols: &'a SymbolIndex,
        scopes: &'a ScopeIndex,
        bindings: &'a BindingIndex,
        inheritance: &'a InheritanceIndex,
        semantics: &'static dyn LanguageSemantics,
    ) -> Self {
        Self {
            file_id,
            file_path,
            module_id,
            language,
            repository,
            symbols,
            scopes,
            bindings,
            inheritance,
            semantics,
        }
    }

    /// What the imports of this file in scope at `scope_id` say about `name`; see
    /// [`scoped_import`].
    pub(crate) fn scoped_import(
        &self,
        scope_id: &ScopeId,
        name: &str,
        is_resolved: impl Fn(&ImportBinding) -> bool,
    ) -> ScopedImport<'a> {
        scoped_import(
            self.repository,
            Some(self.scopes),
            &self.language,
            self.file_id,
            Some(scope_id),
            name,
            is_resolved,
        )
    }
}
