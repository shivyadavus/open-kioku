use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, ReceiverKind, SymbolId, SymbolKind,
};

pub fn resolve_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
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
                let candidates = find_members_by_name(ctx, parent_id, &call.callee_name);

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

                // Try inherited members if direct lookup fails
                if let Some(target) = ctx.inheritance.resolve_inherited_member(
                    parent_id,
                    &call.callee_name,
                    ctx.symbols,
                ) {
                    return ResolutionResult::Resolved {
                        target: target.clone(),
                        confidence: Confidence::Exact,
                        evidence: vec![ResolutionEvidence {
                            kind: ResolutionEvidenceKind::InheritanceGraph,
                            source_type: EvidenceSourceType::TreeSitter,
                            file_range: None,
                            symbol_id: Some(target),
                            message: "resolved self member via inheritance chain".into(),
                        }],
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
    crate::inheritance::resolve_super_member(call, ctx)
}

fn resolve_static_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    let recv = match call.receiver.as_deref() {
        Some(r) => r,
        None => {
            return ResolutionResult::Unresolved {
                reason: UnresolvedReason::UnknownReceiverType,
                evidence: vec![],
            }
        }
    };

    // Resolve the type using file-scoped evidence first (import bindings, same-file types),
    // then fall back to qualified name lookup. Do NOT use global by_name lookup to avoid
    // banned unique-project-name resolution.
    let type_id = resolve_type_with_evidence(ctx, recv);

    if let Some(type_id) = type_id {
        let method_candidates = find_members_by_name(ctx, &type_id, &call.callee_name);

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
        } else if method_candidates.len() > 1 {
            return ResolutionResult::Ambiguous {
                candidates: method_candidates,
                reason: "multiple matching static members".into(),
                evidence: vec![],
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
        None => {
            return ResolutionResult::Unresolved {
                reason: UnresolvedReason::UnknownReceiverType,
                evidence: vec![],
            }
        }
    };

    let binding = match ctx
        .bindings
        .resolve_before(&call.scope_id, recv, &call.range, ctx.scopes)
    {
        Some(b) => b,
        None => {
            return ResolutionResult::Unresolved {
                reason: UnresolvedReason::UnknownReceiverType,
                evidence: vec![],
            }
        }
    };

    let type_name = match binding
        .declared_type
        .as_deref()
        .or(binding.inferred_type.as_deref())
    {
        Some(t) => t,
        None => {
            return ResolutionResult::Unresolved {
                reason: UnresolvedReason::UnknownReceiverType,
                evidence: vec![],
            }
        }
    };

    // Resolve the type using evidence-backed lookup (same file, imports, qualified name),
    // not global unique-name matching.
    let type_id = resolve_type_with_evidence(ctx, type_name);

    if let Some(type_id) = type_id {
        let method_candidates = find_members_by_name(ctx, &type_id, &call.callee_name);

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
        } else if method_candidates.len() > 1 {
            return ResolutionResult::Ambiguous {
                candidates: method_candidates,
                reason: "multiple matching methods on receiver type".into(),
                evidence: vec![],
            };
        }

        // Try inherited members
        if let Some(target) = ctx
            .inheritance
            .resolve_inherited_member(&type_id, &call.callee_name, ctx.symbols)
        {
            return ResolutionResult::Resolved {
                target: target.clone(),
                confidence: Confidence::Exact,
                evidence: vec![ResolutionEvidence {
                    kind: ResolutionEvidenceKind::InheritanceGraph,
                    source_type: EvidenceSourceType::TreeSitter,
                    file_range: None,
                    symbol_id: Some(target),
                    message: "resolved receiver method via inheritance chain".into(),
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
    // Step 1: Lexical scope & enclosing scopes — walk up scope chain
    let mut current_scope_id = Some(call.scope_id.clone());
    while let Some(sid) = current_scope_id {
        let scope_symbols: Vec<SymbolId> = ctx
            .symbols
            .by_file
            .get(ctx.file_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|id| {
                if let Some(sym) = ctx.symbols.get(id) {
                    sym.name == call.callee_name
                        && matches!(sym.kind, SymbolKind::Function | SymbolKind::Method)
                        && sym.scope_id.as_ref() == Some(&sid)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if scope_symbols.len() == 1 {
            return ResolutionResult::Resolved {
                target: scope_symbols[0].clone(),
                confidence: Confidence::Exact,
                evidence: vec![ResolutionEvidence {
                    kind: ResolutionEvidenceKind::LexicalScope,
                    source_type: EvidenceSourceType::TreeSitter,
                    file_range: None,
                    symbol_id: Some(scope_symbols[0].clone()),
                    message: "resolved bare call via lexical scope".into(),
                }],
            };
        } else if scope_symbols.len() > 1 {
            return ResolutionResult::Ambiguous {
                candidates: scope_symbols,
                reason: "multiple bare call candidates in lexical scope".into(),
                evidence: vec![],
            };
        }

        // Walk to enclosing parent scope
        current_scope_id = ctx
            .scopes
            .get(&sid)
            .and_then(|s| s.parent_id.clone());
    }

    // Step 2: Implicit self / containing type check (where language permits)
    if ctx.semantics.implicit_self_dispatch() {
        if let Some(caller_id) = &call.caller_symbol_id {
            if let Some(caller_symbol) = ctx.symbols.get(caller_id) {
                if let Some(parent_id) = &caller_symbol.parent_symbol_id {
                    let self_members = find_members_by_name(ctx, parent_id, &call.callee_name);

                    if self_members.len() == 1 {
                        return ResolutionResult::Resolved {
                            target: self_members[0].clone(),
                            confidence: Confidence::Exact,
                            evidence: vec![ResolutionEvidence {
                                kind: ResolutionEvidenceKind::ImplicitSelf,
                                source_type: EvidenceSourceType::TreeSitter,
                                file_range: None,
                                symbol_id: Some(self_members[0].clone()),
                                message: "resolved bare call via implicit self dispatch".into(),
                            }],
                        };
                    } else if self_members.len() > 1 {
                        return ResolutionResult::Ambiguous {
                            candidates: self_members,
                            reason: "multiple implicit self candidates".into(),
                            evidence: vec![],
                        };
                    }
                }
            }
        }
    }

    // Step 3: Explicit import binding lookup
    let import_bindings =
        ctx.repository
            .imports
            .lookup(ctx.file_id, Some(&call.scope_id), &call.callee_name);

    let resolved_imports: Vec<&SymbolId> = import_bindings
        .iter()
        .filter_map(|imp| imp.target_symbol.as_ref())
        .collect();

    if resolved_imports.len() == 1 {
        let target = resolved_imports[0];
        return ResolutionResult::Resolved {
            target: target.clone(),
            confidence: Confidence::Exact,
            evidence: vec![ResolutionEvidence {
                kind: ResolutionEvidenceKind::ExplicitImport,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: None,
                symbol_id: Some(target.clone()),
                message: "resolved bare call via explicit import binding".into(),
            }],
        };
    } else if resolved_imports.len() > 1 {
        return ResolutionResult::Ambiguous {
            candidates: resolved_imports.into_iter().cloned().collect(),
            reason: "multiple import bindings match".into(),
            evidence: vec![],
        };
    }

    // Step 4: Same file / module check
    let same_file_candidates: Vec<SymbolId> = ctx
        .symbols
        .by_file
        .get(ctx.file_id)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|id| {
            ctx.symbols
                .get(id)
                .map(|s| {
                    s.name == call.callee_name
                        && matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    if same_file_candidates.len() == 1 {
        return ResolutionResult::Resolved {
            target: same_file_candidates[0].clone(),
            confidence: Confidence::High,
            evidence: vec![ResolutionEvidence {
                kind: ResolutionEvidenceKind::SameFile,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: None,
                symbol_id: Some(same_file_candidates[0].clone()),
                message: "resolved bare call via same file function".into(),
            }],
        };
    } else if same_file_candidates.len() > 1 {
        return ResolutionResult::Ambiguous {
            candidates: same_file_candidates,
            reason: "multiple bare call candidates in same file".into(),
            evidence: vec![],
        };
    }

    ResolutionResult::Unresolved {
        reason: UnresolvedReason::NoCandidate,
        evidence: vec![],
    }
}

fn resolve_module_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    // Module members should be resolved through the export index, not as static type members.
    // For now, fall back to static member resolution which checks same-file type members.
    resolve_static_member(call, ctx)
}

/// Resolves a type name to a SymbolId using evidence-backed lookups only.
/// Does NOT use global by_name lookup (which is banned unique-project-name matching).
/// Evidence sources: same-file types, import bindings, qualified name match.
fn resolve_type_with_evidence(ctx: &ResolutionContext<'_>, type_name: &str) -> Option<SymbolId> {
    // 1. Same-file type match
    if let Some(file_symbols) = ctx.symbols.by_file.get(ctx.file_id) {
        let same_file_types: Vec<&SymbolId> = file_symbols
            .iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|s| {
                        s.name == type_name
                            && matches!(
                                s.kind,
                                SymbolKind::Class
                                    | SymbolKind::Trait
                                    | SymbolKind::Interface
                                    | SymbolKind::Module
                            )
                    })
                    .unwrap_or(false)
            })
            .collect();

        if same_file_types.len() == 1 {
            return Some(same_file_types[0].clone());
        }
    }

    // 2. Import binding lookup
    let import_bindings = ctx
        .repository
        .imports
        .lookup(ctx.file_id, None, type_name);
    for binding in &import_bindings {
        if let Some(target) = &binding.target_symbol {
            return Some(target.clone());
        }
    }

    // 3. Qualified name lookup (exact match, not fuzzy)
    if let Some(qualified) = ctx.symbols.by_qualified.get(type_name) {
        if qualified.len() == 1 {
            return Some(qualified[0].clone());
        }
    }

    None
}

/// Helper: find child symbols of a parent with a given name.
/// Avoids cloning the entire Vec by borrowing the slice.
fn find_members_by_name(
    ctx: &ResolutionContext<'_>,
    parent_id: &SymbolId,
    name: &str,
) -> Vec<SymbolId> {
    ctx.symbols
        .by_parent
        .get(parent_id)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|id| {
            ctx.symbols
                .get(id)
                .map(|s| s.name == name)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}
