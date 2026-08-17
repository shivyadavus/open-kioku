from pathlib import Path

path = Path("crates/open-kioku-graph/src/lib.rs")
text = path.read_text()
old = '''    use open_kioku_core::{
        AnalysisFact, Confidence, EdgeId, EvidenceSourceType, File, FileId, FileRange, GraphEdgeType,
        GraphNodeType, Import, Language, LineRange, RelationshipAuthority, RelationshipProof,
        RelationshipProofKind, RepositoryId, SourceRange, Symbol, SymbolId, SymbolKind,
        SymbolOccurrence,
    };
'''
new = '''    use open_kioku_core::{
        AnalysisFact, Confidence, EdgeId, EvidenceSourceType, File, FileId, FileRange,
        GraphEdgeType, GraphNodeType, Import, Language, LineRange, RelationshipAuthority,
        RelationshipProof, RelationshipProofKind, RepositoryId, SourceRange, Symbol, SymbolId,
        SymbolKind, SymbolOccurrence,
    };
'''
observed = text.count(old)
if observed != 1:
    raise SystemExit(f"graph rustfmt seam changed: expected 1, observed {observed}")
path.write_text(text.replace(old, new, 1))
