use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use crate::inheritance::InheritanceIndex;
use open_kioku_core::{Confidence, FileId, Language, ModuleId, ScopeId, SymbolId};
use open_kioku_languages::semantics::LanguageSemantics;
use open_kioku_semantic_model::{ImportBinding, SemanticRepository};

/// Scope id the import registry gives a binding whose site recorded no scope.
const UNSCOPED_IMPORT: &str = "global";

/// Import bindings of `name` in `file_id` declared in `scope_id`, in a scope enclosing it, or at no
/// recorded scope. A binding in a sibling scope, such as `mod tests { use ...; }` seen from
/// production code, is not in scope there. Without a scope index or a use-site scope every binding
/// of the name in the file is returned.
pub(crate) fn visible_import_bindings<'r>(
    repository: &'r SemanticRepository,
    scopes: Option<&ScopeIndex>,
    file_id: &FileId,
    scope_id: Option<&ScopeId>,
    name: &str,
) -> Vec<&'r ImportBinding> {
    let Some(bindings) = repository
        .imports
        .by_file_local_name
        .get(&(file_id.clone(), name.to_string()))
    else {
        return Vec::new();
    };
    let (Some(scopes), Some(scope_id)) = (scopes, scope_id) else {
        return bindings.iter().collect();
    };
    let enclosing = std::iter::successors(Some(scope_id), |id| {
        scopes.get(id).and_then(|scope| scope.parent_id.as_ref())
    })
    .take(scopes.scopes.len() + 1)
    .collect::<Vec<_>>();
    bindings
        .iter()
        .filter(|binding| {
            binding.scope_id.0 == UNSCOPED_IMPORT || enclosing.contains(&&binding.scope_id)
        })
        .collect()
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

    /// Import bindings of `name` in this file visible from `scope_id`.
    pub(crate) fn visible_import_bindings(
        &self,
        scope_id: &ScopeId,
        name: &str,
    ) -> Vec<&'a ImportBinding> {
        visible_import_bindings(
            self.repository,
            Some(self.scopes),
            self.file_id,
            Some(scope_id),
            name,
        )
    }
}
