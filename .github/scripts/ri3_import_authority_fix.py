#!/usr/bin/env python3
from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new)


# 1) Preserve authoritative proof on exact resolved import file edges.
graph_path = Path("crates/open-kioku-graph/src/lib.rs")
graph = graph_path.read_text()
old_graph = '''            buffer.insert_edge(GraphEdge {
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
new_graph = '''            let evidence_id =
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
graph = replace_once(graph, old_graph, new_graph, "graph resolved-import edge")

if "ri3_import_resolution_authority_tests" not in graph:
    graph += r'''

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
graph_path.write_text(graph)


# 2) Authorize exact fully-qualified module calls only when module + member identity agree.
core_path = Path("crates/open-kioku-core/src/relationship.rs")
core = core_path.read_text()
old_policy = '''                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope)))
'''
new_policy = '''                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope))
                    || (module_binding && qualified_name))
'''
core = replace_once(core, old_policy, new_policy, "CALLS module authority policy")

if "exact_module_qualified_call_is_authoritative" not in core:
    insert_before = '''    #[test]
    fn proof_kind_ceiling_prevents_self_promotion() {
'''
    test = '''    #[test]
    fn exact_module_qualified_call_is_authoritative() {
        let proofs = vec![
            proof(RelationshipProofKind::ExactCallSite, 1),
            proof(RelationshipProofKind::ModuleOrPackageBinding, 1),
            proof(RelationshipProofKind::QualifiedName, 1),
        ];
        assert_eq!(
            relationship_authority(&GraphEdgeType::Calls, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

'''
    core = replace_once(core, insert_before, test + insert_before, "core authority regression anchor")
core_path.write_text(core)


# 3) Resolve Rust `crate::module::function()` through deterministic path-qualified symbol names.
typed_path = Path("crates/open-kioku-resolution/src/typed_calls.rs")
typed = typed_path.read_text()
typed = replace_once(
    typed,
    '''    RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
''',
    '''    Language, RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
''',
    "typed calls Language import",
)
old_module = '''pub(crate) fn resolve_module_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let imported = imported_receiver_outcome(call, ctx, receiver);
    match imported {
        ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty() => {
            resolve_named_type_member_outcome(call, ctx, receiver)
        }
        other => other,
    }
}
'''
new_module = '''pub(crate) fn resolve_module_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    if ctx.language == Language::Rust {
        if let Some(outcome) = resolve_rust_qualified_module_outcome(call, ctx, receiver) {
            return outcome;
        }
    }
    let imported = imported_receiver_outcome(call, ctx, receiver);
    match imported {
        ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty() => {
            resolve_named_type_member_outcome(call, ctx, receiver)
        }
        other => other,
    }
}

fn resolve_rust_qualified_module_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> Option<ResolutionOutcome> {
    let module = receiver.strip_prefix("crate::")?;
    if module.is_empty() {
        return None;
    }

    let mut targets = Vec::new();
    for qualified_name in rust_qualified_module_symbol_names(module, &call.callee_name) {
        if let Some(ids) = ctx.symbols.by_qualified.get(&qualified_name) {
            targets.extend(ids.iter().cloned());
        }
    }
    normalize_symbol_ids(&mut targets);
    if targets.is_empty() {
        return None;
    }

    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::LexicalScope,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "candidate from exact Rust crate-qualified module path".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ModuleOrPackageBinding,
                "rust_crate_qualified_module",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "rust_crate_qualified_member",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    Some(evaluate_candidates(&GraphEdgeType::Calls, candidates))
}

fn rust_qualified_module_symbol_names(module: &str, callee: &str) -> [String; 2] {
    [
        format!("src::{module}::{callee}"),
        format!("src::{module}::mod::{callee}"),
    ]
}
'''
typed = replace_once(typed, old_module, new_module, "Rust qualified module resolver")

if "rust_module_symbol_names_match_tree_sitter_qualified_names" not in typed:
    anchor = '''    fn type_symbol(id: &str, name: &str) -> Symbol {
'''
    test = '''    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        assert_eq!(
            rust_qualified_module_symbol_names("storage", "persist"),
            [
                "src::storage::persist".to_string(),
                "src::storage::mod::persist".to_string(),
            ]
        );
    }

'''
    typed = replace_once(typed, anchor, test + anchor, "typed call regression anchor")
typed_path.write_text(typed)
