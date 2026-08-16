use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use crate::inheritance::InheritanceIndex;
use open_kioku_core::{Confidence, FileId, Language, ModuleId, SymbolId};
use open_kioku_languages::semantics::LanguageSemantics;
use open_kioku_semantic_model::SemanticRepository;

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
}
