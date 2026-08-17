from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


replace_exact(
    "crates/open-kioku-graph/src/buffer.rs",
    '''    let number = |key: &str| site.get(key).and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX);\n''',
    '''    let number = |key: &str| {\n        site.get(key)\n            .and_then(serde_json::Value::as_u64)\n            .unwrap_or(u64::MAX)\n    };\n''',
    "structured site number formatting",
)
replace_exact(
    "crates/open-kioku-graph/src/buffer.rs",
    '''        if k != "call_sites"\n            && k != "reference_sites"\n            && k != RELATIONSHIP_PROOFS_PROPERTY\n        {\n''',
    '''        if k != "call_sites" && k != "reference_sites" && k != RELATIONSHIP_PROOFS_PROPERTY {\n''',
    "reference property formatting",
)
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''    identity, AnalysisFact, CodeChunk, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,\n    GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import, LineRange, NodeId,\n    ResolvedRelationship, Symbol, SymbolOccurrence,\n''',
    '''    identity, AnalysisFact, CodeChunk, Confidence, Evidence, EvidenceId, EvidenceSourceType, File,\n    FileRange, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import, LineRange, NodeId,\n    RelationshipProof, RelationshipProofKind, ResolvedRelationship, Symbol, SymbolOccurrence,\n''',
    "exact reference graph imports",
)
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''                    format!(\n                        "{}:{}:{}:{}",\n                        range.start_line,\n                        range.start_column,\n                        range.end_line,\n                        range.end_column\n                    )\n''',
    '''                    format!(\n                        "{}:{}:{}:{}",\n                        range.start_line, range.start_column, range.end_line, range.end_column\n                    )\n''',
    "occurrence key formatting",
)
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''                    proof.details.insert("start_line".into(), json!(range.start_line));\n                    proof\n                        .details\n                        .insert("start_column".into(), json!(range.start_column));\n                    proof.details.insert("end_line".into(), json!(range.end_line));\n''',
    '''                    proof\n                        .details\n                        .insert("start_line".into(), json!(range.start_line));\n                    proof\n                        .details\n                        .insert("start_column".into(), json!(range.start_column));\n                    proof\n                        .details\n                        .insert("end_line".into(), json!(range.end_line));\n''',
    "proof detail formatting",
)
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''        let graph = InMemoryGraph::from_index_with_occurrences(\n            &[file],\n            &[symbol],\n            &[],\n            &[make_occurrence(5), make_occurrence(20)],\n        );\n''',
    '''        let graph = InMemoryGraph::from_index_with_occurrences(\n            &[file.clone()],\n            &[symbol.clone()],\n            &[],\n            &[make_occurrence(5), make_occurrence(20)],\n        );\n''',
    "exact reference test borrow lifetime",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, SourceRange,\n    Symbol, SymbolId, SymbolKind, SymbolOccurrence,\n''',
    '''    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, SourceRange, Symbol,\n    SymbolId, SymbolKind, SymbolOccurrence,\n''',
    "SCIP import formatting",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''}\n\n\n#[cfg(test)]\nmod ri3_exact_reference_tests {\n''',
    '''}\n\n#[cfg(test)]\nmod ri3_exact_reference_tests {\n''',
    "SCIP test spacing",
)
replace_exact(
    "crates/open-kioku-scip/src/lib.rs",
    '''        assert_eq!(occurrences[0].source_range.as_ref().unwrap().start_column, 5);\n        assert_eq!(occurrences[1].source_range.as_ref().unwrap().start_column, 20);\n''',
    '''        assert_eq!(\n            occurrences[0].source_range.as_ref().unwrap().start_column,\n            5\n        );\n        assert_eq!(\n            occurrences[1].source_range.as_ref().unwrap().start_column,\n            20\n        );\n''',
    "SCIP assertion formatting",
)
