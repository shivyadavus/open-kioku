use open_kioku_core::{
    Confidence, EvidenceSourceType, FileRange, GraphEdgeType, SourceRange, SymbolId,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionEvidenceKind {
    LexicalScope,
    TypedBinding,
    ExactImport,
    ExplicitImport,
    ImplicitSelf,
    SameFile,
    InheritedMember,
    InheritanceGraph,
    SCIPOccurrence,
    FallbackHeuristic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionEvidence {
    pub kind: ResolutionEvidenceKind,
    pub source_type: EvidenceSourceType,
    pub file_range: Option<FileRange>,
    pub symbol_id: Option<SymbolId>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRelationship {
    pub from: SymbolId,
    pub to: SymbolId,
    pub edge_type: GraphEdgeType,
    pub confidence: Confidence,
    pub call_site: Option<SourceRange>,
    pub evidence: Vec<ResolutionEvidence>,
}
