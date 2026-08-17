from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


# Explicit import syntax is structural evidence. Attach the typed proof before consumers
# switch to authoritative-only defaults so valid imports do not disappear.
graph = "crates/open-kioku-graph/src/lib.rs"
replace_exact(
    graph,
    '''            let from = identity::file_node_id(&file.path);\n            let edge_id = identity::edge_id(GraphEdgeType::Imports, &from, &target_node.id, None);\n            buffer.insert_edge(GraphEdge {\n                id: edge_id.clone(),\n                from,\n                to: target_node.id,\n                edge_type: GraphEdgeType::Imports,\n                evidence: Evidence {\n                    id: EvidenceId::new(stable_id(&format!("import-evidence:{}", edge_id.0))),\n                    source: "open-kioku-static/imports".into(),\n                    source_type: EvidenceSourceType::StaticAnalysis,\n                    file_range: Some(FileRange {\n                        path: file.path.clone(),\n                        line_range: import.range.clone(),\n                    }),\n                    symbol_id: None,\n                    confidence: import.confidence,\n                    message: format!("{} imports {}", file.path.display(), import.imported),\n                    indexed_at: Utc::now(),\n                    ..Default::default()\n                },\n                ..Default::default()\n            });\n''',
    '''            let from = identity::file_node_id(&file.path);\n            let edge_id = identity::edge_id(GraphEdgeType::Imports, &from, &target_node.id, None);\n            let evidence_id =\n                EvidenceId::new(stable_id(&format!("import-evidence:{}", edge_id.0)));\n            let file_range = FileRange {\n                path: file.path.clone(),\n                line_range: import.range.clone(),\n            };\n            let mut edge = GraphEdge {\n                id: edge_id.clone(),\n                from,\n                to: target_node.id,\n                edge_type: GraphEdgeType::Imports,\n                evidence: Evidence {\n                    id: evidence_id.clone(),\n                    source: "open-kioku-static/imports".into(),\n                    source_type: EvidenceSourceType::StaticAnalysis,\n                    file_range: Some(file_range.clone()),\n                    symbol_id: None,\n                    confidence: import.confidence,\n                    message: format!("{} imports {}", file.path.display(), import.imported),\n                    indexed_at: Utc::now(),\n                    ..Default::default()\n                },\n                ..Default::default()\n            };\n            let mut proof = RelationshipProof::new(\n                RelationshipProofKind::ModuleOrPackageBinding,\n                "static_import_syntax",\n                1,\n            );\n            proof.source_range = Some(file_range);\n            proof.evidence_ids.push(evidence_id);\n            edge.set_relationship_proofs(vec![proof])\n                .expect("static import proof must serialize to JSON");\n            buffer.insert_edge(edge);\n''',
    "static import proof emission",
)
graph_path = Path(graph)
graph_text = graph_path.read_text()
graph_text += r'''

#[cfg(test)]
mod ri3_static_import_authority_tests {
    use super::InMemoryGraph;
    use open_kioku_core::{
        Confidence, File, FileId, GraphEdgeType, Import, Language, LineRange,
        RelationshipProofKind, RepositoryId,
    };
    use std::path::PathBuf;

    #[test]
    fn explicit_static_import_emits_authoritative_module_binding_proof() {
        let file = File {
            id: FileId::new("file:src/lib.rs"),
            repository_id: RepositoryId::new("repo:test"),
            path: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let import = Import {
            file_id: file.id.clone(),
            imported: "crate::domain".into(),
            range: Some(LineRange::single(3)),
            confidence: Confidence::Exact,
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            std::slice::from_ref(&file),
            &[],
            &[],
            &[],
            &[import],
            &[],
        );
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == GraphEdgeType::Imports)
            .expect("explicit import should emit an imports edge");

        assert!(edge.is_authoritative_relationship());
        assert!(edge.has_relationship_proof_kind(RelationshipProofKind::ModuleOrPackageBinding));
        assert_eq!(edge.relationship_proofs().len(), 1);
    }
}
'''
graph_path.write_text(graph_text)

# Architecture policy is a structural consumer: raw/legacy heuristic edges must not become
# policy violations once proof-gated emission exists.
architecture = "crates/open-kioku-architecture/src/lib.rs"
replace_exact(
    architecture,
    '''const MAX_UNKNOWN_EDGE_SAMPLES: usize = 100;\n\n''',
    '''const MAX_UNKNOWN_EDGE_SAMPLES: usize = 100;\n\nfn is_authoritative_policy_edge(edge: &GraphEdge) -> bool {\n    edge.is_authoritative_relationship()\n}\n\n''',
    "architecture authority helper",
)
replace_exact(
    architecture,
    '''            for edge in &batch {\n                report.evaluated_edge_count += 1;\n                evaluate_edge(\n''',
    '''            for edge in &batch {\n                if !is_authoritative_policy_edge(edge) {\n                    continue;\n                }\n                report.evaluated_edge_count += 1;\n                evaluate_edge(\n''',
    "architecture policy authoritative default",
)
replace_exact(
    architecture,
    '''            for edge in &batch {\n                if let Some(evidence) = edge_evidence(\n''',
    '''            for edge in &batch {\n                if !is_authoritative_policy_edge(edge) {\n                    continue;\n                }\n                if let Some(evidence) = edge_evidence(\n''',
    "public API authoritative default",
)
replace_exact(
    architecture,
    '''            .push("no import, reference, or call graph edges were available to evaluate".into());\n''',
    '''            .push("no authoritative import, reference, or call graph edges were available to evaluate".into());\n''',
    "architecture uncertainty wording",
    2,
)
architecture_path = Path(architecture)
architecture_text = architecture_path.read_text()
architecture_text += r'''

#[cfg(test)]
mod ri3_authoritative_policy_edge_tests {
    use super::is_authoritative_policy_edge;
    use open_kioku_core::{
        GraphEdge, GraphEdgeType, RelationshipProof, RelationshipProofKind,
    };

    #[test]
    fn policy_edges_fail_closed_without_typed_authority() {
        let mut edge = GraphEdge {
            edge_type: GraphEdgeType::Imports,
            ..GraphEdge::default()
        };
        assert!(!is_authoritative_policy_edge(&edge));

        let proof = RelationshipProof::new(
            RelationshipProofKind::ModuleOrPackageBinding,
            "test_static_import",
            1,
        );
        edge.set_relationship_proofs(vec![proof]).unwrap();
        assert!(is_authoritative_policy_edge(&edge));
    }
}
'''
architecture_path.write_text(architecture_text)

# Context still needs ordinary graph structure (Defines/Contains/Tests/etc.), so filter only the
# RI3 proof-gated relationship families. Raw graph inspection APIs intentionally remain unchanged.
context = "crates/open-kioku-context/src/lib.rs"
replace_exact(
    context,
    '''pub mod candidates;\npub mod routing;\n\n''',
    '''pub mod candidates;\npub mod routing;\n\nfn is_trusted_context_dependency_edge(edge: &GraphEdge) -> bool {\n    match &edge.edge_type {\n        GraphEdgeType::Calls\n        | GraphEdgeType::References\n        | GraphEdgeType::UsesType\n        | GraphEdgeType::Implements\n        | GraphEdgeType::Extends\n        | GraphEdgeType::Imports\n        | GraphEdgeType::DependsOn => edge.is_authoritative_relationship(),\n        _ => true,\n    }\n}\n\n''',
    "context authority helper",
)
replace_exact(
    context,
    '''            if let Ok((_nodes, edges)) = self.store.neighbors(&node_id, 20) {\n                dependency_edges.extend(edges);\n            }\n''',
    '''            if let Ok((_nodes, edges)) = self.store.neighbors(&node_id, 20) {\n                dependency_edges.extend(\n                    edges\n                        .into_iter()\n                        .filter(is_trusted_context_dependency_edge),\n                );\n            }\n''',
    "context authoritative dependency expansion",
)
context_path = Path(context)
context_text = context_path.read_text()
context_text += r'''

#[cfg(test)]
mod ri3_context_dependency_authority_tests {
    use super::is_trusted_context_dependency_edge;
    use open_kioku_core::{
        GraphEdge, GraphEdgeType, RelationshipProof, RelationshipProofKind,
    };

    #[test]
    fn proof_gated_context_edges_fail_closed_but_ordinary_graph_structure_remains_available() {
        let ordinary = GraphEdge {
            edge_type: GraphEdgeType::Defines,
            ..GraphEdge::default()
        };
        assert!(is_trusted_context_dependency_edge(&ordinary));

        let mut import = GraphEdge {
            edge_type: GraphEdgeType::Imports,
            ..GraphEdge::default()
        };
        assert!(!is_trusted_context_dependency_edge(&import));

        let proof = RelationshipProof::new(
            RelationshipProofKind::ModuleOrPackageBinding,
            "test_static_import",
            1,
        );
        import.set_relationship_proofs(vec![proof]).unwrap();
        assert!(is_trusted_context_dependency_edge(&import));
    }
}
'''
context_path.write_text(context_text)
