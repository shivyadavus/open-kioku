use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{
    evaluate_candidates, ResolutionCandidate, ResolutionOutcome, ResolutionStrategy,
};
use crate::type_candidates::{discover_type_candidates, discovery_candidate_count, TypeDiscovery};
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, GraphEdgeType, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
};
use std::collections::BTreeMap;

pub fn resolve_call_outcome(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionOutcome {
    let candidates = match call.receiver_kind {
        open_kioku_core::ReceiverKind::Self_ => discover_self_candidates(call, ctx),
        open_kioku_core::ReceiverKind::Super => discover_super_candidates(call, ctx),
        open_kioku_core::ReceiverKind::Type => discover_typed_or_static_candidates(
            call,
            ctx,
            call.receiver.as_deref(),
            ResolutionStrategy::StaticReceiver,
        ),
        open_kioku_core::ReceiverKind::Value => discover_value_candidates(call, ctx),
        open_kioku_core::ReceiverKind::None => discover_bare_candidates(call, ctx),
        open_kioku_core::ReceiverKind::Module => discover_module_candidates(call, ctx),
        open_kioku_core::ReceiverKind::Unknown => Vec::new(),
    };
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

fn discover_value_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(receiver) = call.receiver.as_deref() else {
        return Vec::new();
    };
    let lookup_name = receiver
        .trim_start_matches("this.")
        .trim_start_matches("self.")
        .trim_start_matches("Self::");

    if let Some(binding) =
        ctx.bindings
            .resolve_before(&call.scope_id, lookup_name, &call.range, ctx.scopes)
    {
        if let Some(type_name) = binding
            .declared_type
            .as_deref()
            .or(binding.inferred_type.as_deref())
        {
            return discover_typed_or_static_candidates(
                call,
                ctx,
                Some(type_name),
                ResolutionStrategy::TypedReceiver,
            );
        }
    }

    discover_imported_member_candidates(call, ctx, lookup_name)
}

fn discover_self_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(caller_id) = call.caller_symbol_id.as_ref() else {
        return Vec::new();
    };
    let Some(caller) = ctx.symbols.get(caller_id) else {
        return Vec::new();
    };
    let Some(containing_type) = caller.parent_symbol_id.as_ref() else {
        return Vec::new();
    };

    let receiver = call.receiver.as_deref().unwrap_or_default();
    let field_name = receiver
        .trim_start_matches("this.")
        .trim_start_matches("self.")
        .trim_start_matches("Self::");
    if !field_name.is_empty()
        && !matches!(field_name, "this" | "self" | "Self")
        && field_name != receiver
    {
        let field_candidates = members_by_name(ctx, containing_type, field_name, true);
        let mut type_names = field_candidates
            .iter()
            .filter_map(|field_id| ctx.symbols.get(field_id))
            .filter_map(|field| field.signature.clone())
            .collect::<Vec<_>>();
        if let Some(binding) =
            ctx.bindings
                .resolve_before(&call.scope_id, field_name, &call.range, ctx.scopes)
        {
            if let Some(type_name) = binding
                .declared_type
                .as_ref()
                .or(binding.inferred_type.as_ref())
            {
                type_names.push(type_name.clone());
            }
        }
        type_names.sort();
        type_names.dedup();
        let mut candidates = Vec::new();
        for type_name in type_names {
            candidates.extend(discover_typed_or_static_candidates(
                call,
                ctx,
                Some(&type_name),
                ResolutionStrategy::TypedReceiver,
            ));
        }
        if !candidates.is_empty() {
            return candidates;
        }
    }

    let targets = members_by_name(ctx, containing_type, &call.callee_name, false);
    candidates_for_targets(
        call,
        ctx,
        targets,
        CandidateTemplate {
            confidence: Confidence::Exact,
            strategy: ResolutionStrategy::ImplicitSelf,
            proof_kinds: &[
                RelationshipProofKind::ReceiverType,
                RelationshipProofKind::ContainingType,
                RelationshipProofKind::SameScopeDefinition,
            ],
            evidence_kind: ResolutionEvidenceKind::ImplicitSelf,
            message: "resolved explicit self/this member from containing type",
        },
    )
}

fn discover_super_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(caller_id) = call.caller_symbol_id.as_ref() else {
        return Vec::new();
    };
    let Some(caller) = ctx.symbols.get(caller_id) else {
        return Vec::new();
    };
    let Some(containing_type) = caller.parent_symbol_id.as_ref() else {
        return Vec::new();
    };
    let targets = ctx.inheritance.inherited_member_candidates(
        containing_type,
        &call.callee_name,
        ctx.symbols,
    );
    candidates_for_targets(
        call,
        ctx,
        targets,
        CandidateTemplate {
            confidence: Confidence::Exact,
            strategy: ResolutionStrategy::Inheritance,
            proof_kinds: &[
                RelationshipProofKind::InheritanceBinding,
                RelationshipProofKind::ContainingType,
            ],
            evidence_kind: ResolutionEvidenceKind::InheritanceGraph,
            message: "super-call candidate discovered from nearest inheritance binding",
        },
    )
}

fn discover_typed_or_static_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_name: Option<&str>,
    strategy: ResolutionStrategy,
) -> Vec<ResolutionCandidate> {
    let Some(type_name) = type_name else {
        return Vec::new();
    };
    let type_candidates = discover_type_candidates(
        ctx.file_id,
        Some(&call.scope_id),
        type_name,
        ctx.repository,
        ctx.symbols,
    );
    if type_candidates.is_empty() {
        return discover_imported_member_candidates(call, ctx, type_name);
    }

    let receiver_candidate_count = type_candidates.len();
    let import_candidate_count =
        discovery_candidate_count(&type_candidates, TypeDiscovery::ImportBinding);
    let qualified_candidate_count =
        discovery_candidate_count(&type_candidates, TypeDiscovery::QualifiedName);
    let mut targets = BTreeMap::<String, (SymbolId, Vec<TypeDiscovery>)>::new();
    for type_candidate in type_candidates {
        for target in members_by_name(ctx, &type_candidate.target, &call.callee_name, false) {
            let entry = targets
                .entry(target.0.clone())
                .or_insert_with(|| (target.clone(), Vec::new()));
            entry.1.extend(type_candidate.discoveries.iter().copied());
        }
    }
    let target_count = targets.len();
    targets
        .into_values()
        .map(|(target, mut discoveries)| {
            discoveries.sort();
            discoveries.dedup();
            let mut candidate =
                ResolutionCandidate::new(target.clone(), Confidence::Exact).with_strategy(strategy);
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                call,
                ctx,
                &target,
                RelationshipProofKind::ReceiverType,
                "receiver_type",
                receiver_candidate_count,
            ));
            candidate.proofs.push(proof(
                call,
                ctx,
                &target,
                RelationshipProofKind::SameScopeDefinition,
                "member_on_resolved_receiver_type",
                target_count,
            ));
            for discovery in discoveries {
                match discovery {
                    TypeDiscovery::ImportBinding => candidate.proofs.push(proof(
                        call,
                        ctx,
                        &target,
                        RelationshipProofKind::ImportBinding,
                        "receiver_type_import_binding",
                        import_candidate_count,
                    )),
                    TypeDiscovery::QualifiedName => candidate.proofs.push(proof(
                        call,
                        ctx,
                        &target,
                        RelationshipProofKind::QualifiedName,
                        "receiver_type_qualified_name",
                        qualified_candidate_count,
                    )),
                    TypeDiscovery::SameFile => {}
                }
            }
            candidate.evidence.push(resolution_evidence(
                call,
                ctx,
                target,
                ResolutionEvidenceKind::TypedBinding,
                "method candidate discovered from uniquely typed receiver",
            ));
            candidate
        })
        .collect()
}

fn discover_module_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(receiver) = call.receiver.as_deref() else {
        return Vec::new();
    };
    discover_imported_member_candidates(call, ctx, receiver)
}

fn discover_imported_member_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> Vec<ResolutionCandidate> {
    let mut targets = BTreeMap::<String, SymbolId>::new();
    for binding in ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(&call.scope_id), receiver)
    {
        if let Some(module_id) = &binding.resolved_module {
            for export in ctx.repository.exports.lookup(module_id, &call.callee_name) {
                if let Some(target) = &export.origin_symbol {
                    targets.insert(target.0.clone(), target.clone());
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
                            targets.insert(target.0.clone(), target.clone());
                        }
                    }
                }
            }
            if let Some(file_symbols) = ctx.symbols.by_file.get(target_file) {
                for target in file_symbols {
                    if ctx
                        .symbols
                        .get(target)
                        .map(|symbol| {
                            symbol.name == call.callee_name && symbol.parent_symbol_id.is_none()
                        })
                        .unwrap_or(false)
                    {
                        targets.insert(target.0.clone(), target.clone());
                    }
                }
            }
        }
    }
    let count = targets.len();
    targets
        .into_values()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact)
                .with_strategy(ResolutionStrategy::ExactImportBinding)
                .with_strategy(ResolutionStrategy::ModuleExport);
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                call,
                ctx,
                &target,
                RelationshipProofKind::ImportBinding,
                "exact_import_binding",
                count,
            ));
            candidate.proofs.push(proof(
                call,
                ctx,
                &target,
                RelationshipProofKind::QualifiedName,
                "exact_imported_member_identity",
                count,
            ));
            candidate.evidence.push(resolution_evidence(
                call,
                ctx,
                target,
                ResolutionEvidenceKind::ExplicitImport,
                "member candidate discovered from exact import/module binding",
            ));
            candidate
        })
        .collect()
}

fn discover_bare_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let mut current_scope = Some(call.scope_id.clone());
    let mut visited = std::collections::HashSet::new();
    while let Some(scope_id) = current_scope {
        if !visited.insert(scope_id.clone()) {
            break;
        }
        let targets = ctx
            .symbols
            .by_file
            .get(ctx.file_id)
            .map(|symbols| {
                symbols
                    .iter()
                    .filter(|id| {
                        ctx.symbols
                            .get(id)
                            .map(|symbol| {
                                symbol.name == call.callee_name
                                    && matches!(
                                        symbol.kind,
                                        SymbolKind::Function | SymbolKind::Method
                                    )
                                    && symbol.scope_id.as_ref() == Some(&scope_id)
                            })
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !targets.is_empty() {
            // This proof remains corroborating under the current core CALLS policy. #238 will only
            // elevate it after a dedicated lexical-shadowing test matrix proves the language rule.
            return candidates_for_targets(
                call,
                ctx,
                targets,
                CandidateTemplate {
                    confidence: Confidence::Exact,
                    strategy: ResolutionStrategy::LexicalScope,
                    proof_kinds: &[RelationshipProofKind::SameScopeDefinition],
                    evidence_kind: ResolutionEvidenceKind::LexicalScope,
                    message: "bare-call candidate discovered in exact lexical scope",
                },
            );
        }
        current_scope = ctx
            .scopes
            .get(&scope_id)
            .and_then(|scope| scope.parent_id.clone());
    }

    if ctx.semantics.implicit_self_dispatch() {
        let implicit = discover_self_candidates(call, ctx);
        if !implicit.is_empty() {
            return implicit;
        }
    }

    let import_targets = ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(&call.scope_id), &call.callee_name)
        .into_iter()
        .filter_map(|binding| binding.target_symbol.clone())
        .collect::<Vec<_>>();
    if !import_targets.is_empty() {
        let count = import_targets.len();
        return import_targets
            .into_iter()
            .map(|target| {
                let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact)
                    .with_strategy(ResolutionStrategy::ExactImportBinding);
                candidate.proofs.push(call_site_proof(call, ctx, &target));
                candidate.proofs.push(proof(
                    call,
                    ctx,
                    &target,
                    RelationshipProofKind::ImportBinding,
                    "bare_exact_import_binding",
                    count,
                ));
                candidate.proofs.push(proof(
                    call,
                    ctx,
                    &target,
                    RelationshipProofKind::QualifiedName,
                    "bare_exact_import_target",
                    count,
                ));
                candidate.evidence.push(resolution_evidence(
                    call,
                    ctx,
                    target,
                    ResolutionEvidenceKind::ExplicitImport,
                    "bare-call candidate discovered from exact import binding",
                ));
                candidate
            })
            .collect();
    }

    if ctx.language != open_kioku_core::Language::Java {
        let targets = ctx
            .symbols
            .by_file
            .get(ctx.file_id)
            .map(|symbols| {
                symbols
                    .iter()
                    .filter(|id| {
                        ctx.symbols
                            .get(id)
                            .map(|symbol| {
                                symbol.name == call.callee_name
                                    && symbol.parent_symbol_id.is_none()
                                    && symbol.kind == SymbolKind::Function
                            })
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let count = targets.len();
        return targets
            .into_iter()
            .map(|target| {
                let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::High)
                    .with_strategy(ResolutionStrategy::SameFile);
                candidate.evidence.push(resolution_evidence(
                    call,
                    ctx,
                    target,
                    ResolutionEvidenceKind::SameFile,
                    &format!(
                        "same-file candidate retained as heuristic among {count} candidate(s)"
                    ),
                ));
                candidate
            })
            .collect();
    }

    Vec::new()
}

fn members_by_name(
    ctx: &ResolutionContext<'_>,
    parent: &SymbolId,
    name: &str,
    fields_only: bool,
) -> Vec<SymbolId> {
    ctx.symbols
        .by_parent
        .get(parent)
        .map(|members| {
            members
                .iter()
                .filter(|id| {
                    ctx.symbols
                        .get(id)
                        .map(|symbol| {
                            symbol.name == name
                                && if fields_only {
                                    symbol.kind == SymbolKind::Field
                                } else {
                                    matches!(symbol.kind, SymbolKind::Method | SymbolKind::Function)
                                }
                        })
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

struct CandidateTemplate<'a> {
    confidence: Confidence,
    strategy: ResolutionStrategy,
    proof_kinds: &'a [RelationshipProofKind],
    evidence_kind: ResolutionEvidenceKind,
    message: &'a str,
}

fn candidates_for_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
    template: CandidateTemplate<'_>,
) -> Vec<ResolutionCandidate> {
    let count = targets.len();
    targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), template.confidence)
                .with_strategy(template.strategy);
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            for kind in template.proof_kinds {
                candidate.proofs.push(proof(
                    call,
                    ctx,
                    &target,
                    *kind,
                    strategy_name(template.strategy),
                    count,
                ));
            }
            candidate.evidence.push(resolution_evidence(
                call,
                ctx,
                target,
                template.evidence_kind.clone(),
                template.message,
            ));
            candidate
        })
        .collect()
}

fn strategy_name(strategy: ResolutionStrategy) -> &'static str {
    match strategy {
        ResolutionStrategy::ExactOccurrence => "exact_occurrence",
        ResolutionStrategy::LexicalScope => "lexical_scope",
        ResolutionStrategy::ImplicitSelf => "implicit_self",
        ResolutionStrategy::TypedReceiver => "typed_receiver",
        ResolutionStrategy::StaticReceiver => "static_receiver",
        ResolutionStrategy::ExactImportBinding => "exact_import_binding",
        ResolutionStrategy::ModuleExport => "module_export",
        ResolutionStrategy::SameFile => "same_file",
        ResolutionStrategy::Inheritance => "inheritance",
        ResolutionStrategy::QualifiedName => "qualified_name",
        ResolutionStrategy::ExternalExactIndex => "external_exact_index",
        ResolutionStrategy::Heuristic => "heuristic",
    }
}

// Kept separate so all call proof paths preserve the same exact source occurrence metadata.
fn call_site_proof(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
) -> RelationshipProof {
    proof(
        call,
        ctx,
        target,
        RelationshipProofKind::ExactCallSite,
        "exact_call_site",
        1,
    )
}

fn proof(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
    kind: RelationshipProofKind,
    strategy: &str,
    candidate_count: usize,
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = Some(call_file_range(call, ctx));
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof.evidence_ids.push(EvidenceId::new(call.id.0.clone()));
    proof
}

fn resolution_evidence(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: SymbolId,
    kind: ResolutionEvidenceKind,
    message: &str,
) -> ResolutionEvidence {
    ResolutionEvidence {
        kind,
        source_type: EvidenceSourceType::TreeSitter,
        file_range: Some(call_file_range(call, ctx)),
        symbol_id: Some(target),
        message: message.into(),
    }
}

fn call_file_range(call: &CallSite, ctx: &ResolutionContext<'_>) -> FileRange {
    FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: call.range.start_line,
            end: call.range.end_line,
        }),
    }
}
