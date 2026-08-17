from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

path = Path("crates/open-kioku-graph/src/lib.rs")
text = path.read_text()

old = '''    identity, normalize_relationship_proofs, AnalysisFact, CodeChunk, Evidence, EvidenceId,
    EvidenceSourceType, File, FileRange, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType,
    Import, LineRange, NodeId, ResolvedRelationship, Symbol, SymbolOccurrence,
    RELATIONSHIP_PROOFS_PROPERTY,
'''
new = '''    identity, normalize_relationship_proofs, AnalysisFact, CodeChunk, Confidence, Evidence,
    EvidenceId, EvidenceSourceType, File, FileRange, GraphEdge, GraphEdgeType, GraphNode,
    GraphNodeType, Import, LineRange, NodeId, RelationshipProof, RelationshipProofKind,
    ResolvedRelationship, Symbol, SymbolOccurrence, RELATIONSHIP_PROOFS_PROPERTY,
'''
text = replace_exact(text, old, new, "graph exact reference imports")

old = '''            let from = identity::file_node_id(&file.path);
            let to = identity::symbol_node_id(symbol);
            buffer.insert_edge(GraphEdge {
                id: identity::edge_id(GraphEdgeType::References, &from, &to, None),
                from,
                to,
                edge_type: GraphEdgeType::References,
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!(
                        "occurrence-evidence:{}:{}",
                        file.id.0, symbol.id.0
                    ))),
                    source: "open-kioku-graph".into(),
                    source_type: occurrence.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: occurrence.confidence,
                    message: format!("{} references {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
'''
new = '''            let from = identity::file_node_id(&file.path);
            let to = identity::symbol_node_id(symbol);
            let occurrence_key = occurrence
                .range
                .as_ref()
                .map(|range| format!("{}:{}", range.start, range.end))
                .unwrap_or_else(|| "unknown-range".into());
            let occurrence_evidence_id = EvidenceId::new(stable_id(&format!(
                "occurrence-evidence:{}:{}:{}",
                file.id.0, symbol.id.0, occurrence_key
            )));
            let mut edge = GraphEdge {
                id: identity::edge_id(GraphEdgeType::References, &from, &to, None),
                from,
                to,
                edge_type: GraphEdgeType::References,
                evidence: Evidence {
                    id: occurrence_evidence_id.clone(),
                    source: "open-kioku-graph".into(),
                    source_type: occurrence.provenance.clone(),
                    file_range: Some(FileRange {
                        path: file.path.clone(),
                        line_range: occurrence.range.clone(),
                    }),
                    symbol_id: Some(symbol.id.clone()),
                    confidence: occurrence.confidence,
                    message: format!("{} references {}", file.path.display(), symbol.name),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            };
            if occurrence.confidence == Confidence::Exact {
                let mut proof = RelationshipProof::new(
                    RelationshipProofKind::ExactOccurrence,
                    "exact_symbol_occurrence",
                    1,
                );
                proof.source_range = Some(FileRange {
                    path: file.path.clone(),
                    line_range: occurrence.range.clone(),
                });
                proof.target_symbol_id = Some(symbol.id.clone());
                proof.evidence_ids = vec![occurrence_evidence_id];
                let _ = edge.set_relationship_proofs(vec![proof]);
            }
            buffer.insert_edge(edge);
'''
text = replace_exact(text, old, new, "exact occurrence reference emission")

anchor = '''    #[test]
    fn shortest_path_finds_route() {
'''
test = '''    #[test]
    fn exact_reference_occurrences_merge_as_authoritative_proofs() {
        let file = make_file("refs");
        let symbol = make_symbol("target", "refs", "target");
        let occurrences = vec![
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: file.id.clone(),
                range: Some(LineRange::single(5)),
                is_definition: false,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: file.id.clone(),
                range: Some(LineRange::single(9)),
                is_definition: false,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
        ];

        let graph = InMemoryGraph::from_index_with_occurrences(
            &[file],
            &[symbol],
            &[],
            &occurrences,
        );
        let references = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == GraphEdgeType::References)
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1);
        assert!(references[0].is_authoritative_relationship());
        let proofs = references[0].relationship_proofs();
        assert_eq!(proofs.len(), 2);
        assert!(proofs
            .iter()
            .all(|proof| proof.kind == RelationshipProofKind::ExactOccurrence));
        assert_eq!(
            proofs
                .iter()
                .filter_map(|proof| proof
                    .source_range
                    .as_ref()
                    .and_then(|range| range.line_range.as_ref())
                    .map(|range| range.start))
                .collect::<Vec<_>>(),
            vec![5, 9]
        );
        assert_ne!(proofs[0].evidence_ids, proofs[1].evidence_ids);
    }

    #[test]
    fn non_exact_reference_occurrence_remains_heuristic() {
        let file = make_file("heuristic-ref");
        let symbol = make_symbol("target", "heuristic-ref", "target");
        let occurrence = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: file.id.clone(),
            range: Some(LineRange::single(5)),
            is_definition: false,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        };

        let graph = InMemoryGraph::from_index_with_occurrences(
            &[file],
            &[symbol],
            &[],
            &[occurrence],
        );
        let reference = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == GraphEdgeType::References)
            .expect("reference edge should exist");
        assert_eq!(
            reference.relationship_authority(),
            open_kioku_core::RelationshipAuthority::Heuristic
        );
        assert!(reference.relationship_proofs().is_empty());
    }

'''
text = replace_exact(text, anchor, test + anchor, "exact reference tests")
path.write_text(text)
