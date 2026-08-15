use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use open_kioku_core::{CallSite, Confidence, EvidenceSourceType, ReceiverKind, SymbolId, SymbolKind};

pub fn resolve_call(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionResult {
    match call.receiver_kind {
        ReceiverKind::Self_ => resolve_self_member(call, ctx),
        ReceiverKind::Super => resolve_super_member(call, ctx),
        ReceiverKind::Type => resolve_static_member(call, ctx),
        ReceiverKind::Value => resolve_typed_receiver(call, ctx),
        ReceiverKind::None => resolve_bare_call(call, ctx),
        ReceiverKind::Module => resolve_module_member(call, ctx),
        ReceiverKind::Unknown => ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnsupportedDynamicDispatch,
            evidence: vec![],
        },
    }
}

fn resolve_self_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    if let Some(caller_id) = &call.caller_symbol_id {
        if let Some(caller_symbol) = ctx.symbols.get(caller_id) {
            if let Some(parent_id) = &caller_symbol.parent_symbol_id {
                let candidates: Vec<SymbolId> = ctx
                    .symbols
                    .by_parent
                    .get(parent_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|id| {
                        ctx.symbols
                            .get(id)
                            .map(|s| s.name == call.callee_name)
                            .unwrap_or(false)
                    })
                    .collect();

                if candidates.len() == 1 {
                    return ResolutionResult::Resolved {
                        target: candidates[0].clone(),
                        confidence: Confidence::Exact,
                        evidence: vec![ResolutionEvidence {
                            kind: ResolutionEvidenceKind::LexicalScope,
                            source_type: EvidenceSourceType::TreeSitter,
                            file_range: None,
                            symbol_id: Some(candidates[0].clone()),
                            message: "resolved via self/this member lookup".into(),
                        }],
                    };
                } else if candidates.len() > 1 {
                    return ResolutionResult::Ambiguous {
                        candidates,
                        reason: "multiple matching methods on self/this type".into(),
                        evidence: vec![],
                    };
                }
            }
        }
    }
    ResolutionResult::Unresolved {
        reason: UnresolvedReason::NoCandidate,
        evidence: vec![],
    }
}

fn resolve_super_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    resolve_self_member(call, ctx)
}

fn resolve_static_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    let recv = match call.receiver.as_deref() {
        Some(r) => r,
        None => return ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnknownReceiverType,
            evidence: vec![],
        },
    };

    let type_candidates = ctx.symbols.lookup_name(recv);
    if type_candidates.len() == 1 {
        let type_id = &type_candidates[0];
        let method_candidates: Vec<SymbolId> = ctx
            .symbols
            .by_parent
            .get(type_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|s| s.name == call.callee_name)
                    .unwrap_or(false)
            })
            .collect();

        if method_candidates.len() == 1 {
            return ResolutionResult::Resolved {
                target: method_candidates[0].clone(),
                confidence: Confidence::Exact,
                evidence: vec![ResolutionEvidence {
                    kind: ResolutionEvidenceKind::TypedBinding,
                    source_type: EvidenceSourceType::TreeSitter,
                    file_range: None,
                    symbol_id: Some(method_candidates[0].clone()),
                    message: "resolved static member call".into(),
                }],
            };
        }
    }

    ResolutionResult::Unresolved {
        reason: UnresolvedReason::NoCandidate,
        evidence: vec![],
    }
}

fn resolve_typed_receiver(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    let recv = match call.receiver.as_deref() {
        Some(r) => r,
        None => return ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnknownReceiverType,
            evidence: vec![],
        },
    };

    let binding = match ctx.bindings.resolve_before(&call.scope_id, recv, &call.range, ctx.scopes) {
        Some(b) => b,
        None => return ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnknownReceiverType,
            evidence: vec![],
        },
    };

    let type_name = match binding.declared_type.as_deref().or(binding.inferred_type.as_deref()) {
        Some(t) => t,
        None => return ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnknownReceiverType,
            evidence: vec![],
        },
    };

    let type_candidates = ctx.symbols.lookup_name(type_name);
    if type_candidates.len() == 1 {
        let type_id = &type_candidates[0];
        let method_candidates: Vec<SymbolId> = ctx
            .symbols
            .by_parent
            .get(type_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|s| s.name == call.callee_name)
                    .unwrap_or(false)
            })
            .collect();

        if method_candidates.len() == 1 {
            return ResolutionResult::Resolved {
                target: method_candidates[0].clone(),
                confidence: Confidence::Exact,
                evidence: vec![ResolutionEvidence {
                    kind: ResolutionEvidenceKind::TypedBinding,
                    source_type: EvidenceSourceType::TreeSitter,
                    file_range: None,
                    symbol_id: Some(method_candidates[0].clone()),
                    message: "resolved method via typed local variable binding".into(),
                }],
            };
        }
    }

    ResolutionResult::Unresolved {
        reason: UnresolvedReason::UnknownReceiverType,
        evidence: vec![],
    }
}

fn resolve_bare_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    let candidates = ctx.symbols.lookup_name(&call.callee_name);
    if candidates.len() == 1 {
        if let Some(sym) = ctx.symbols.get(&candidates[0]) {
            if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                return ResolutionResult::Resolved {
                    target: candidates[0].clone(),
                    confidence: Confidence::High,
                    evidence: vec![ResolutionEvidence {
                        kind: ResolutionEvidenceKind::LexicalScope,
                        source_type: EvidenceSourceType::TreeSitter,
                        file_range: None,
                        symbol_id: Some(candidates[0].clone()),
                        message: "resolved bare function call".into(),
                    }],
                };
            }
        }
    } else if candidates.len() > 1 {
        return ResolutionResult::Ambiguous {
            candidates: candidates.to_vec(),
            reason: "multiple bare function candidates".into(),
            evidence: vec![],
        };
    }

    ResolutionResult::Unresolved {
        reason: UnresolvedReason::NoCandidate,
        evidence: vec![],
    }
}

fn resolve_module_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    resolve_static_member(call, ctx)
}
