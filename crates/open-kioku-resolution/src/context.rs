use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use crate::inheritance::InheritanceIndex;
use open_kioku_core::{Confidence, FileId, Language, ModuleId, ScopeId, ScopeKind, SymbolId};
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

    let mut current = Some(scope_id);
    let mut steps = 0usize;
    while let Some(id) = current {
        let here = named
            .iter()
            .copied()
            .filter(|binding| &binding.scope_id == id)
            .collect::<Vec<_>>();
        if !here.is_empty() {
            return settle(here);
        }
        if globs.iter().any(|binding| &binding.scope_id == id) {
            return ScopedImport::Unresolved;
        }
        let scope = scopes.get(id);
        if *language == Language::Rust && scope.is_some_and(|scope| scope.kind == ScopeKind::Module)
        {
            return ScopedImport::NotImported;
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
