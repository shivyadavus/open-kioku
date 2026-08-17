from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


# First-class, backward-compatible proof transport on resolved relationships.
replace_exact(
    "crates/open-kioku-core/src/lib.rs",
    '''    #[serde(default)]
    pub evidence: Vec<ResolutionEvidence>,
}\n''',
    '''    #[serde(default)]
    pub evidence: Vec<ResolutionEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proofs: Vec<RelationshipProof>,
}\n''',
    "ResolvedRelationship proof field",
)

# Ingest consumes the proof-preserving outcome directly. Only Proven can emit a structural edge.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''                        let v2_result = open_kioku_resolution::resolve_call(call, &ctx);
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
''',
    '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
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
''',
    "proof-preserving ingest resolution",
)

# Graph materialization copies typed proofs rather than reconstructing authority from messages.
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''                source_pass: Some("open-kioku-resolution".into()),
                ambiguity: Vec::new(),
                evidence: Evidence {
''',
    '''                source_pass: Some("open-kioku-resolution".into()),
                ambiguity: Vec::new(),
                relationship_proofs: rel.proofs.clone(),
                evidence: Evidence {
''',
    "graph proof materialization",
)

# Existing test literals explicitly model proofless legacy relationships.
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''                evidence: Vec::new(),
            },
''',
    '''                evidence: Vec::new(),
                proofs: Vec::new(),
            },
''',
    "first graph relationship literal",
    count=2,
)

# Strengthen the same-line call-site regression with real typed proofs and authority checks.
replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''    use open_kioku_core::{
        AnalysisFact, Confidence, EdgeId, EvidenceSourceType, File, FileId, GraphEdgeType,
        GraphNodeType, Import, Language, LineRange, RepositoryId, SourceRange, Symbol, SymbolId,
        SymbolKind, SymbolOccurrence,
    };
''',
    '''    use open_kioku_core::{
        AnalysisFact, Confidence, EdgeId, EvidenceSourceType, File, FileId, FileRange, GraphEdgeType,
        GraphNodeType, Import, Language, LineRange, RelationshipAuthority, RelationshipProof,
        RelationshipProofKind, RepositoryId, SourceRange, Symbol, SymbolId, SymbolKind,
        SymbolOccurrence,
    };
''',
    "graph test imports",
)

replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''    #[test]
    fn resolved_relationships_preserve_same_line_call_columns() {
        let file = make_file("src/service");
        let caller = make_symbol("caller", "src/service", "run");
        let callee = make_symbol("callee", "src/service", "save");
        let relationships = vec![
''',
    '''    fn call_relationship_proofs(
        caller: &SymbolId,
        callee: &SymbolId,
        start_line: u32,
    ) -> Vec<RelationshipProof> {
        let range = Some(FileRange {
            path: PathBuf::from("src/service.rs"),
            line_range: Some(LineRange {
                start: start_line,
                end: start_line,
            }),
        });
        let mut call_site =
            RelationshipProof::new(RelationshipProofKind::ExactCallSite, "test_call_site", 1);
        call_site.source_range = range.clone();
        call_site.source_symbol_id = Some(caller.clone());
        call_site.target_symbol_id = Some(callee.clone());

        let mut target = RelationshipProof::new(
            RelationshipProofKind::SameScopeDefinition,
            "test_same_scope",
            1,
        );
        target.source_range = range;
        target.source_symbol_id = Some(caller.clone());
        target.target_symbol_id = Some(callee.clone());
        vec![call_site, target]
    }

    #[test]
    fn resolved_relationships_preserve_same_line_call_columns() {
        let file = make_file("src/service");
        let caller = make_symbol("caller", "src/service", "run");
        let callee = make_symbol("callee", "src/service", "save");
        let relationships = vec![
''',
    "graph proof test helper",
)

# Replace proofless values in these two literals with occurrence-specific proof sets.
text_path = Path("crates/open-kioku-graph/src/lib.rs")
text = text_path.read_text()
needle = '''                evidence: Vec::new(),
                proofs: Vec::new(),
            },
'''
first = '''                evidence: Vec::new(),
                proofs: call_relationship_proofs(&caller.id, &callee.id, 10),
            },
'''
if text.count(needle) != 2:
    raise SystemExit(f"graph proof literal seam changed: expected 2, observed {text.count(needle)}")
text = text.replace(needle, first, 2)
text_path.write_text(text)

replace_exact(
    "crates/open-kioku-graph/src/lib.rs",
    '''        assert_eq!(sites[0]["start_column"], 5);
        assert_eq!(sites[1]["start_column"], 20);
    }
''',
    '''        assert_eq!(sites[0]["start_column"], 5);
        assert_eq!(sites[1]["start_column"], 20);
        assert_eq!(calls[0].relationship_proofs.len(), 2);
        assert_eq!(
            calls[0].relationship_authority(),
            RelationshipAuthority::Authoritative
        );
        assert!(calls[0]
            .relationship_proofs
            .iter()
            .all(|proof| proof.target_symbol_id.as_ref() == Some(&callee.id)));
    }
''',
    "merged relationship proof assertion",
)

# GraphBuffer must merge typed proof/evidence identity when logical edges collapse.
replace_exact(
    "crates/open-kioku-graph/src/buffer.rs",
    '''use open_kioku_core::{
    identity, EdgeId, Evidence, EvidenceSourceType, GraphEdge, GraphEdgeType, GraphNode, NodeId,
};
''',
    '''use open_kioku_core::{
    identity, normalize_relationship_proofs, EdgeId, Evidence, EvidenceSourceType, GraphEdge,
    GraphEdgeType, GraphNode, NodeId,
};
''',
    "buffer proof-normalization import",
)

replace_exact(
    "crates/open-kioku-graph/src/buffer.rs",
    '''    existing.quality_notes.extend(incoming.quality_notes);
    existing.quality_notes.sort();
    existing.quality_notes.dedup();

    if existing.schema_version.is_none() {
''',
    '''    existing.quality_notes.extend(incoming.quality_notes);
    existing.quality_notes.sort();
    existing.quality_notes.dedup();

    existing
        .relationship_proofs
        .extend(incoming.relationship_proofs);
    existing.relationship_proofs =
        normalize_relationship_proofs(std::mem::take(&mut existing.relationship_proofs));
    existing.evidence_ids.extend(incoming.evidence_ids);
    existing.evidence_ids.sort();
    existing.evidence_ids.dedup();

    if existing.schema_version.is_none() {
''',
    "buffer proof merge",
)
