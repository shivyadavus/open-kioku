from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

# 1. Carry typed proof sets on the existing resolved-relationship domain object. Serde default keeps
# old serialized relationships readable; resolver function signatures remain unchanged.
core = Path("crates/open-kioku-core/src/lib.rs")
text = core.read_text()
old = '''    #[serde(default)]
    pub evidence: Vec<ResolutionEvidence>,
}
'''
new = '''    #[serde(default)]
    pub evidence: Vec<ResolutionEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proofs: Vec<RelationshipProof>,
}
'''
text = replace_exact(text, old, new, "ResolvedRelationship proof field")
core.write_text(text)

# 2. Give every call proof a stable occurrence evidence id plus exact column metadata. FileRange
# retains lines; columns remain inspectable here and in the graph's call_sites property.
calls = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = calls.read_text()
old = '''    CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
'''
new = '''    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, GraphEdgeType, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
'''
text = replace_exact(text, old, new, "call candidate EvidenceId import")
old = '''    proof.source_range = Some(call_file_range(call, ctx));
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof
}
'''
new = '''    proof.source_range = Some(call_file_range(call, ctx));
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof.evidence_ids.push(EvidenceId::new(call.id.0.clone()));
    proof.details.insert(
        "start_column".into(),
        serde_json::Value::from(call.range.start_column),
    );
    proof.details.insert(
        "end_column".into(),
        serde_json::Value::from(call.range.end_column),
    );
    proof
}
'''
text = replace_exact(text, old, new, "call proof occurrence metadata")
calls.write_text(text)

# 3. Ingest now treats the proof-gated outcome as structural truth. Ambiguous/unresolved candidates
# remain diagnostic only and do not enter resolved_relationships.
ingest = Path("crates/open-kioku-ingest/src/lib.rs")
text = ingest.read_text()
old = '''                        let v2_result = open_kioku_resolution::resolve_call(call, &ctx);
                        let semantic_target = match &v2_result {
                            open_kioku_resolution::ResolutionResult::Resolved {
                                target,
                                confidence,
                                evidence,
                            } => {
                                match confidence {
                                    Confidence::Exact => quality_report.resolved_exact += 1,
                                    Confidence::High => quality_report.resolved_high += 1,
                                    _ => {}
                                }
                                if let Some(caller) = &call.caller_symbol_id {
                                    resolved_relationships.push(
                                        open_kioku_resolution::ResolvedRelationship {
                                            from: caller.clone(),
                                            to: target.clone(),
                                            edge_type: GraphEdgeType::Calls,
                                            confidence: *confidence,
                                            call_site: Some(call.range.clone()),
                                            evidence: evidence.clone(),
                                        },
                                    );
                                }
                                Some(target.clone())
                            }
                            open_kioku_resolution::ResolutionResult::Ambiguous { .. } => {
                                quality_report.ambiguous += 1;
                                None
                            }
                            open_kioku_resolution::ResolutionResult::External { .. } => None,
                            open_kioku_resolution::ResolutionResult::Unresolved { .. } => {
                                quality_report.unresolved += 1;
                                None
                            }
                        };
'''
new = '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        let semantic_target = match &v2_outcome {
                            open_kioku_resolution::ResolutionOutcome::Proven { candidate } => {
                                match candidate.confidence {
                                    Confidence::Exact => quality_report.resolved_exact += 1,
                                    Confidence::High => quality_report.resolved_high += 1,
                                    _ => {}
                                }
                                if let Some(caller) = &call.caller_symbol_id {
                                    resolved_relationships.push(
                                        open_kioku_resolution::ResolvedRelationship {
                                            from: caller.clone(),
                                            to: candidate.target_symbol_id.clone(),
                                            edge_type: GraphEdgeType::Calls,
                                            confidence: candidate.confidence,
                                            call_site: Some(call.range.clone()),
                                            evidence: candidate.evidence.clone(),
                                            proofs: candidate.proofs.clone(),
                                        },
                                    );
                                }
                                Some(candidate.target_symbol_id.clone())
                            }
                            open_kioku_resolution::ResolutionOutcome::Ambiguous { .. } => {
                                quality_report.ambiguous += 1;
                                None
                            }
                            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                                quality_report.external += 1;
                                None
                            }
                            open_kioku_resolution::ResolutionOutcome::Unresolved { .. } => {
                                quality_report.unresolved += 1;
                                None
                            }
                        };
'''
text = replace_exact(text, old, new, "ingest proof-gated call outcome")
ingest.write_text(text)

# 4. Graph emission serializes the typed proof set through the same canonical core representation.
graph = Path("crates/open-kioku-graph/src/lib.rs")
text = graph.read_text()
old = '''    identity, AnalysisFact, CodeChunk, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,
    GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import, LineRange, NodeId,
    ResolvedRelationship, Symbol, SymbolOccurrence,
'''
new = '''    identity, normalize_relationship_proofs, AnalysisFact, CodeChunk, Evidence, EvidenceId,
    EvidenceSourceType, File, FileRange, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import,
    LineRange, NodeId, ResolvedRelationship, Symbol, SymbolOccurrence,
    RELATIONSHIP_PROOFS_PROPERTY,
'''
text = replace_exact(text, old, new, "graph proof imports")
anchor = '''            buffer.insert_edge(GraphEdge {
                id: edge_id.clone(),
                from: from_node,
'''
insert = '''            if !rel.proofs.is_empty() {
                if let Ok(value) =
                    serde_json::to_value(normalize_relationship_proofs(rel.proofs.clone()))
                {
                    properties.insert(RELATIONSHIP_PROOFS_PROPERTY.to_string(), value);
                }
            }

            buffer.insert_edge(GraphEdge {
                id: edge_id.clone(),
                from: from_node,
'''
text = replace_exact(text, anchor, insert, "graph resolved relationship proof serialization")

# Existing same-line call-site regression gets distinct proof occurrences so it also verifies proof
# merge behavior on one logical edge.
old = '''        AnalysisFact, Confidence, EdgeId, EvidenceSourceType, File, FileId, GraphEdgeType,
        GraphNodeType, Import, Language, LineRange, RepositoryId, SourceRange, Symbol, SymbolId,
        SymbolKind, SymbolOccurrence,
'''
new = '''        AnalysisFact, Confidence, EdgeId, EvidenceId, EvidenceSourceType, File, FileId,
        GraphEdgeType, GraphNodeType, Import, Language, LineRange, RelationshipProof,
        RelationshipProofKind, RepositoryId, SourceRange, Symbol, SymbolId, SymbolKind,
        SymbolOccurrence,
'''
text = replace_exact(text, old, new, "graph test proof imports")
old = '''        let callee = make_symbol("callee", "src/service", "save");
        let relationships = vec![
'''
new = '''        let callee = make_symbol("callee", "src/service", "save");
        let proofs_for = |evidence_id: &str| {
            let mut call_site =
                RelationshipProof::new(RelationshipProofKind::ExactCallSite, "test_call_site", 1);
            call_site.source_symbol_id = Some(caller.id.clone());
            call_site.target_symbol_id = Some(callee.id.clone());
            call_site.evidence_ids = vec![EvidenceId::new(evidence_id)];
            let mut same_scope = RelationshipProof::new(
                RelationshipProofKind::SameScopeDefinition,
                "test_same_scope",
                1,
            );
            same_scope.source_symbol_id = Some(caller.id.clone());
            same_scope.target_symbol_id = Some(callee.id.clone());
            same_scope.evidence_ids = vec![EvidenceId::new(evidence_id)];
            vec![call_site, same_scope]
        };
        let relationships = vec![
'''
text = replace_exact(text, old, new, "graph proof fixture")
old = '''                }),
                evidence: Vec::new(),
            },
            ResolvedRelationship {
'''
new = '''                }),
                evidence: Vec::new(),
                proofs: proofs_for("call-site-a"),
            },
            ResolvedRelationship {
'''
text = replace_exact(text, old, new, "first graph relationship proof")
old = '''                    end_column: 26,
                }),
                evidence: Vec::new(),
            },
'''
new = '''                    end_column: 26,
                }),
                evidence: Vec::new(),
                proofs: proofs_for("call-site-b"),
            },
'''
text = replace_exact(text, old, new, "second graph relationship proof")
old = '''        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["start_column"], 5);
        assert_eq!(sites[1]["start_column"], 20);
    }
'''
new = '''        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["start_column"], 5);
        assert_eq!(sites[1]["start_column"], 20);
        assert!(calls[0].is_authoritative_relationship());
        let proofs = calls[0].relationship_proofs();
        assert_eq!(proofs.len(), 4);
        let proof_evidence = proofs
            .iter()
            .flat_map(|proof| proof.evidence_ids.iter().map(|id| id.0.as_str()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(proof_evidence, std::collections::BTreeSet::from(["call-site-a", "call-site-b"]));
    }
'''
text = replace_exact(text, old, new, "graph multiple call proof assertion")
graph.write_text(text)

# 5. GraphBuffer must merge typed proof arrays deterministically when multiple call occurrences
# collapse into one logical edge. Malformed proof metadata removes the trust property entirely.
buffer = Path("crates/open-kioku-graph/src/buffer.rs")
text = buffer.read_text()
old = '''use open_kioku_core::{
    identity, EdgeId, Evidence, EvidenceSourceType, GraphEdge, GraphEdgeType, GraphNode, NodeId,
};
'''
new = '''use open_kioku_core::{
    identity, EdgeId, Evidence, EvidenceSourceType, GraphEdge, GraphEdgeType, GraphNode, NodeId,
    RELATIONSHIP_PROOFS_PROPERTY,
};
'''
text = replace_exact(text, old, new, "GraphBuffer proof import")
anchor = '''fn merge_edge_metadata(existing: &mut GraphEdge, incoming: GraphEdge) {
'''
helper = '''fn merge_relationship_proofs(existing: &mut GraphEdge, incoming: &GraphEdge) {
    let carries_proofs = existing
        .properties
        .contains_key(RELATIONSHIP_PROOFS_PROPERTY)
        || incoming
            .properties
            .contains_key(RELATIONSHIP_PROOFS_PROPERTY);
    if !carries_proofs {
        return;
    }

    let (Ok(mut proofs), Ok(incoming_proofs)) = (
        existing.try_relationship_proofs(),
        incoming.try_relationship_proofs(),
    ) else {
        existing.properties.remove(RELATIONSHIP_PROOFS_PROPERTY);
        return;
    };
    proofs.extend(incoming_proofs);
    if existing.set_relationship_proofs(proofs).is_err() {
        existing.properties.remove(RELATIONSHIP_PROOFS_PROPERTY);
    }
}

fn merge_edge_metadata(existing: &mut GraphEdge, incoming: GraphEdge) {
    merge_relationship_proofs(existing, &incoming);
'''
text = replace_exact(text, anchor, helper, "GraphBuffer proof merge helper")
old = '''    for (k, v) in incoming.properties {
        if k != "call_sites" {
            existing.properties.entry(k).or_insert(v);
        }
    }
'''
new = '''    for (k, v) in incoming.properties {
        if k != "call_sites" && k != RELATIONSHIP_PROOFS_PROPERTY {
            existing.properties.entry(k).or_insert(v);
        }
    }
'''
text = replace_exact(text, old, new, "GraphBuffer proof property exclusion")
buffer.write_text(text)
