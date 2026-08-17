#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/open-kioku-graph/src/lib.rs")
text = path.read_text()

old = '''            buffer.insert_edge(GraphEdge {
                id: edge_id.clone(),
                from: source_node,
                to: target_node.id,
                edge_type: fact.edge_type.clone(),
                properties: analysis_edge_properties(fact),
                source_pass: Some(fact.source.clone()),
                ambiguity: analysis_fact_ambiguity(fact),
                evidence: Evidence {
                    id: EvidenceId::new(stable_id(&format!("analysis-evidence:{}", edge_id.0))),
                    source: fact.source.clone(),
                    source_type: fact.source_type.clone(),
                    file_range: fact.range.as_ref().map(|range| FileRange {
                        path: file.path.clone(),
                        line_range: Some(range.clone()),
                    }),
                    symbol_id: fact.symbol_id.clone(),
                    confidence: fact.confidence,
                    message: fact.message.clone(),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            });
'''
new = '''            let evidence_id =
                EvidenceId::new(stable_id(&format!("analysis-evidence:{}", edge_id.0)));
            let mut edge = GraphEdge {
                id: edge_id.clone(),
                from: source_node,
                to: target_node.id,
                edge_type: fact.edge_type.clone(),
                properties: analysis_edge_properties(fact),
                source_pass: Some(fact.source.clone()),
                ambiguity: analysis_fact_ambiguity(fact),
                evidence: Evidence {
                    id: evidence_id.clone(),
                    source: fact.source.clone(),
                    source_type: fact.source_type.clone(),
                    file_range: fact.range.as_ref().map(|range| FileRange {
                        path: file.path.clone(),
                        line_range: Some(range.clone()),
                    }),
                    symbol_id: fact.symbol_id.clone(),
                    confidence: fact.confidence,
                    message: fact.message.clone(),
                    indexed_at: Utc::now(),
                    ..Default::default()
                },
                ..Default::default()
            };
            if fact.edge_type == GraphEdgeType::Imports
                && fact.target_kind == GraphNodeType::File
                && fact.source.starts_with("open-kioku-import-resolver/")
                && matches!(fact.confidence, Confidence::High | Confidence::Exact)
            {
                let mut proof = RelationshipProof::new(
                    RelationshipProofKind::ImportBinding,
                    fact.source.clone(),
                    1,
                );
                proof.source_range = fact.range.as_ref().map(|range| FileRange {
                    path: file.path.clone(),
                    line_range: Some(range.clone()),
                });
                proof.evidence_ids.push(evidence_id);
                proof.details.insert("target_path".into(), json!(fact.target));
                edge.set_relationship_proofs(vec![proof])
                    .expect("resolved import binding proof must serialize to JSON");
            }
            buffer.insert_edge(edge);
'''

if text.count(old) != 1:
    raise SystemExit(f"expected one analysis-fact edge block, found {text.count(old)}")
text = text.replace(old, new)

marker = "ri3_import_resolution_authority_tests"
if marker not in text:
    text += r'''

#[cfg(test)]
mod ri3_import_resolution_authority_tests {
    use super::InMemoryGraph;
    use open_kioku_core::{
        AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType, GraphNodeType,
        Language, LineRange, RelationshipProofKind, RepositoryId,
    };
    use std::path::PathBuf;

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo:test"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: format!("hash:{path}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    #[test]
    fn resolved_import_analysis_fact_is_authoritative() {
        let source = file("source", "src/domain/mod.rs");
        let target = file("target", "src/api/internal/mod.rs");
        let fact = AnalysisFact {
            id: "resolved-import".into(),
            file_id: source.id.clone(),
            symbol_id: None,
            target: target.path.to_string_lossy().into_owned(),
            target_kind: GraphNodeType::File,
            edge_type: GraphEdgeType::Imports,
            range: Some(LineRange::single(1)),
            confidence: Confidence::High,
            source: "open-kioku-import-resolver/rust-module".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "resolved import to an exact repository file".into(),
        };

        let graph = InMemoryGraph::from_index_with_analysis(
            &[source.clone(), target.clone()],
            &[],
            &[],
            &[],
            &[],
            &[fact],
        );
        let target_node = open_kioku_core::identity::file_node_id(&target.path);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == GraphEdgeType::Imports && edge.to == target_node)
            .expect("resolved import should target the concrete repository file");

        assert!(edge.is_authoritative_relationship());
        assert!(edge.has_relationship_proof_kind(RelationshipProofKind::ImportBinding));
    }

    #[test]
    fn unresolved_import_analysis_fact_remains_untrusted() {
        let source = file("source", "src/domain/mod.rs");
        let fact = AnalysisFact {
            id: "unresolved-import".into(),
            file_id: source.id.clone(),
            symbol_id: None,
            target: "crate::missing".into(),
            target_kind: GraphNodeType::Module,
            edge_type: GraphEdgeType::Imports,
            range: Some(LineRange::single(1)),
            confidence: Confidence::Low,
            source: "open-kioku-import-resolver/unresolved".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "unresolved import".into(),
        };

        let graph =
            InMemoryGraph::from_index_with_analysis(&[source], &[], &[], &[], &[], &[fact]);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == GraphEdgeType::Imports)
            .expect("unresolved import should remain inspectable evidence");

        assert!(!edge.is_authoritative_relationship());
        assert!(!edge.has_relationship_proof_kind(RelationshipProofKind::ImportBinding));
    }
}
'''

path.write_text(text)
