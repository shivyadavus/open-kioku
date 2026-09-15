use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use crate::inheritance::InheritanceIndex;
use open_kioku_core::{
    Confidence, FileId, Language, ModuleId, Scope, ScopeId, ScopeKind, SymbolId,
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
