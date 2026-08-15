use crate::evidence::ResolutionEvidence;
use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
use open_kioku_core::{Confidence, SymbolId};

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
    pub symbols: &'a SymbolIndex,
    pub scopes: &'a ScopeIndex,
    pub bindings: &'a BindingIndex,
}

impl<'a> ResolutionContext<'a> {
    pub fn new(
        symbols: &'a SymbolIndex,
        scopes: &'a ScopeIndex,
        bindings: &'a BindingIndex,
    ) -> Self {
        Self {
            symbols,
            scopes,
            bindings,
        }
    }
}
