use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, FileRange, LineRange, ReceiverKind, SymbolId,
    SymbolKind,
};

fn call_file_range(call: &CallSite, ctx: &ResolutionContext<'_>) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: call.range.start_line,
            end: call.range.end_line,
        }),
    })
}

pub fn resolve_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    match call.receiver_kind {
        ReceiverKind::Self_ => resolve_self_member(call, ctx),
        ReceiverKind::Super => resolve_super_member(call, ctx),
        ReceiverKind::Type => {
            crate::typed_calls::resolve_static_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Value => {
            crate::typed_calls::resolve_typed_receiver_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::None => {
            crate::bare_calls::resolve_bare_call_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Module => {
            crate::typed_calls::resolve_module_member_outcome(call, ctx).into_legacy_result()
        }
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
                // If receiver has a member path like "this.repo" or "self.repo", resolve field first
                if let Some(recv) = call.receiver.as_deref() {
                    let stripped = recv
                        .trim_start_matches("this.")
                        .trim_start_matches("self.")
                        .trim_start_matches("Self::");
                    if !stripped.is_empty()
                        && stripped != "this"
                        && stripped != "self"
                        && stripped != "Self"
                    {
                        // Look up field on parent class
                        let field_candidates = find_members_by_name(ctx, parent_id, stripped);
                        for fid in field_candidates {
                            if let Some(field_sym) = ctx.symbols.get(&fid) {
                                if let Some(field_type) = &field_sym.signature {
                                    if let Some(target_type_id) =
                                        resolve_type_with_evidence(ctx, field_type)
                                    {
                                        let method_candidates = find_members_by_name(
                                            ctx,
                                            &target_type_id,
                                            &call.callee_name,
                                        );
                                        if method_candidates.len() == 1 {
                                            return ResolutionResult::Resolved {
                                                target: method_candidates[0].clone(),
                                                confidence: Confidence::Exact,
                                                evidence: vec![ResolutionEvidence {
                                                    kind: ResolutionEvidenceKind::TypedBinding,
                                                    source_type: EvidenceSourceType::TreeSitter,
                                                    file_range: call_file_range(call, ctx),
                                                    symbol_id: Some(method_candidates[0].clone()),
                                                    message:
                                                        "resolved method via self field member"
                                                            .into(),
                                                }],
                                            };
                                        }
                                    }
                                }
                            }
                        }
                        // Also try looking up field binding in BindingIndex
                        if let Some(binding) = ctx.bindings.resolve_before(
                            &call.scope_id,
                            stripped,
                            &call.range,
                            ctx.scopes,
                        ) {
                            if let Some(type_name) = binding
                                .declared_type
                                .as_deref()
                                .or(binding.inferred_type.as_deref())
                            {
                                if let Some(target_type_id) =
                                    resolve_type_with_evidence(ctx, type_name)
                                {
                                    let method_candidates = find_members_by_name(
                                        ctx,
                                        &target_type_id,
                                        &call.callee_name,
                                    );
                                    if method_candidates.len() == 1 {
                                        return ResolutionResult::Resolved {
                                            target: method_candidates[0].clone(),
                                            confidence: Confidence::Exact,
                                            evidence: vec![ResolutionEvidence {
                                                kind: ResolutionEvidenceKind::TypedBinding,
                                                source_type: EvidenceSourceType::TreeSitter,
                                                file_range: call_file_range(call, ctx),
                                                symbol_id: Some(method_candidates[0].clone()),
                                                message: "resolved method via self field binding"
                                                    .into(),
                                            }],
                                        };
                                    }
                                }
                            }
                        }
                    }
                }

                let candidates = find_members_by_name(ctx, parent_id, &call.callee_name);

                if candidates.len() == 1 {
                    return ResolutionResult::Resolved {
                        target: candidates[0].clone(),
                        confidence: Confidence::Exact,
                        evidence: vec![ResolutionEvidence {
                            kind: ResolutionEvidenceKind::LexicalScope,
                            source_type: EvidenceSourceType::TreeSitter,
                            file_range: call_file_range(call, ctx),
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
                            file_range: call_file_range(call, ctx),
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

    // 2. Import binding lookup - if ambiguous, return None rather than taking first match
    let import_bindings = ctx.repository.imports.lookup(ctx.file_id, None, type_name);
    let matching_targets: Vec<&SymbolId> = import_bindings
        .iter()
        .filter_map(|b| b.target_symbol.as_ref())
        .collect();
    if matching_targets.len() == 1 {
        return Some(matching_targets[0].clone());
    } else if matching_targets.len() > 1 {
        return None;
    }

    // 2b. If target_file is known on import binding, look up matching type in that file.
    // Collect across all bindings so multiple valid targets fail closed instead of first-match wins.
    let mut file_candidates = Vec::new();
    for b in &import_bindings {
        if let Some(target_file_id) = &b.target_file {
            if let Some(file_syms) = ctx.symbols.by_file.get(target_file_id) {
                for id in file_syms {
                    if ctx
                        .symbols
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
                    {
                        file_candidates.push(id.clone());
                    }
                }
            }
        }
    }
    file_candidates.sort_by(|a, b| a.0.cmp(&b.0));
    file_candidates.dedup();
    if file_candidates.len() == 1 {
        return Some(file_candidates[0].clone());
    } else if file_candidates.len() > 1 {
        return None;
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
        .filter(|id| ctx.symbols.get(id).map(|s| s.name == name).unwrap_or(false))
        .cloned()
        .collect()
}
