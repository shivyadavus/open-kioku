use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
};
use std::collections::BTreeMap;

pub(crate) fn resolve_typed_receiver_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let lookup_name = receiver
        .trim_start_matches("this.")
        .trim_start_matches("self.")
        .trim_start_matches("Self::");

    let Some(binding) = ctx
        .bindings
        .resolve_before(&call.scope_id, lookup_name, &call.range, ctx.scopes)
    else {
        return imported_receiver_outcome(call, ctx, lookup_name);
    };

    let Some(type_name) = binding
        .declared_type
        .as_deref()
        .or(binding.inferred_type.as_deref())
    else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };

    let type_candidates = collect_type_candidates(ctx, type_name);
    if type_candidates.is_empty() {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    let mut direct_targets = Vec::new();
    for type_id in &type_candidates {
        direct_targets.extend(find_members_by_name(ctx, type_id, &call.callee_name));
    }
    normalize_symbol_ids(&mut direct_targets);
    if !direct_targets.is_empty() {
        return evaluate_direct_member_targets(call, ctx, direct_targets);
    }

    // The inheritance index currently exposes only a single traversal winner per receiver type.
    // Retain those targets as corroborating candidates until inheritance resolution exposes the
    // complete candidate set; traversal order must never create structural truth.
    let mut inherited_targets = Vec::new();
    for type_id in &type_candidates {
        if let Some(target) =
            ctx.inheritance
                .resolve_inherited_member(type_id, &call.callee_name, ctx.symbols)
        {
            inherited_targets.push(target);
        }
    }
    normalize_symbol_ids(&mut inherited_targets);
    if inherited_targets.is_empty() {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    evaluate_inherited_targets(call, ctx, inherited_targets)
}

fn imported_receiver_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> ResolutionOutcome {
    let mut targets = Vec::new();
    let import_bindings =
        ctx.repository
            .imports
            .lookup(ctx.file_id, Some(&call.scope_id), receiver);

    for binding in import_bindings {
        if let Some(module_id) = &binding.resolved_module {
            for export in ctx.repository.exports.lookup(module_id, &call.callee_name) {
                if let Some(target) = &export.origin_symbol {
                    targets.push(target.clone());
                }
            }
        }
        if let Some(target_file) = &binding.target_file {
            for ((_, exported_name), exports) in &ctx.repository.exports.by_module_exported_name {
                if exported_name != &call.callee_name {
                    continue;
                }
                for export in exports {
                    if &export.file_id == target_file {
                        if let Some(target) = &export.origin_symbol {
                            targets.push(target.clone());
                        }
                    }
                }
            }
            if let Some(file_symbols) = ctx.symbols.by_file.get(target_file) {
                for id in file_symbols {
                    if ctx
                        .symbols
                        .get(id)
                        .map(|symbol| {
                            symbol.name == call.callee_name && symbol.parent_symbol_id.is_none()
                        })
                        .unwrap_or(false)
                    {
                        targets.push(id.clone());
                    }
                }
            }
        }
    }
    normalize_symbol_ids(&mut targets);
    if targets.is_empty() {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::ExplicitImport,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "receiver call candidate from exact import/module binding".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ImportBinding,
                "receiver_import_binding",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "receiver_import_member",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

fn evaluate_direct_member_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
) -> ResolutionOutcome {
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "method candidate from typed receiver binding".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ReceiverType,
                "typed_receiver",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::ContainingType,
                "direct_member_of_receiver_type",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

fn evaluate_inherited_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
) -> ResolutionOutcome {
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::InheritanceGraph,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "inherited receiver candidate retained without authoritative uniqueness"
                    .into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::InheritanceBinding,
                "receiver_inheritance_candidate",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

fn collect_type_candidates(ctx: &ResolutionContext<'_>, type_name: &str) -> Vec<SymbolId> {
    let mut candidates = BTreeMap::<String, SymbolId>::new();

    if let Some(file_symbols) = ctx.symbols.by_file.get(ctx.file_id) {
        for id in file_symbols {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| is_type_symbol(symbol.kind.clone()) && symbol.name == type_name)
                .unwrap_or(false)
            {
                candidates.insert(id.0.clone(), id.clone());
            }
        }
    }

    for binding in ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(&ctx.scopes.innermost_scope_for_file(ctx.file_id).unwrap_or_else(|| open_kioku_core::ScopeId::new(""))), type_name)
    {
        if let Some(target) = &binding.target_symbol {
            if ctx
                .symbols
                .get(target)
                .map(|symbol| is_type_symbol(symbol.kind.clone()))
                .unwrap_or(false)
            {
                candidates.insert(target.0.clone(), target.clone());
            }
        }
        if let Some(target_file) = &binding.target_file {
            if let Some(file_symbols) = ctx.symbols.by_file.get(target_file) {
                for id in file_symbols {
                    if ctx
                        .symbols
                        .get(id)
                        .map(|symbol| is_type_symbol(symbol.kind.clone()) && symbol.name == type_name)
                        .unwrap_or(false)
                    {
                        candidates.insert(id.0.clone(), id.clone());
                    }
                }
            }
        }
    }

    if let Some(qualified) = ctx.symbols.by_qualified.get(type_name) {
        for id in qualified {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| is_type_symbol(symbol.kind.clone()))
                .unwrap_or(false)
            {
                candidates.insert(id.0.clone(), id.clone());
            }
        }
    }

    candidates.into_values().collect()
}

fn is_type_symbol(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
}

fn find_members_by_name(
    ctx: &ResolutionContext<'_>,
    parent_id: &SymbolId,
    name: &str,
) -> Vec<SymbolId> {
    let mut candidates = ctx
        .symbols
        .by_parent
        .get(parent_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|id| {
            ctx.symbols
                .get(id)
                .map(|symbol| symbol.name == name)
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    normalize_symbol_ids(&mut candidates);
    candidates
}

fn normalize_symbol_ids(ids: &mut Vec<SymbolId>) {
    ids.sort_by(|left, right| left.0.cmp(&right.0));
    ids.dedup();
}

fn ambiguity_strings(ids: &[SymbolId]) -> Vec<String> {
    if ids.len() > 1 {
        ids.iter().map(|id| id.0.clone()).collect()
    } else {
        Vec::new()
    }
}

fn call_site_proof(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(RelationshipProofKind::ExactCallSite, "call_site", 1);
    proof.source_range = call_file_range(call, ctx);
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof
}

#[allow(clippy::too_many_arguments)]
fn proof(
    kind: RelationshipProofKind,
    strategy: &str,
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
    candidate_count: usize,
    ambiguity: &[String],
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = call_file_range(call, ctx);
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof.ambiguity = ambiguity.to_vec();
    proof
}

fn call_file_range(call: &CallSite, ctx: &ResolutionContext<'_>) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: call.range.start_line,
            end: call.range.end_line,
        }),
    })
}
