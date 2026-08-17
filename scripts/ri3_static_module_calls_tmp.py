from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)


typed = Path("crates/open-kioku-resolution/src/typed_calls.rs")
text = typed.read_text()
old_block = '''    let type_candidates = collect_type_candidates(ctx, &call.scope_id, type_name);
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
'''
new_block = '''    resolve_named_type_member_outcome(call, ctx, type_name)
}

pub(crate) fn resolve_static_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let typed = resolve_named_type_member_outcome(call, ctx, receiver);
    match typed {
        ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty() => {
            imported_receiver_outcome(call, ctx, receiver)
        }
        other => other,
    }
}

pub(crate) fn resolve_module_member_outcome(
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

pub(crate) fn resolve_named_type_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_name: &str,
) -> ResolutionOutcome {
    let type_candidates = collect_type_candidates(ctx, &call.scope_id, type_name);
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

pub(crate) fn imported_receiver_outcome(
'''
text = replace_exact(text, old_block, new_block, "typed/static/module helper extraction")
typed.write_text(text)

calls = Path("crates/open-kioku-resolution/src/calls.rs")
text = calls.read_text()
text = replace_exact(
    text,
    "        ReceiverKind::Type => resolve_static_member(call, ctx),\n",
    "        ReceiverKind::Type => {\n            crate::typed_calls::resolve_static_member_outcome(call, ctx).into_legacy_result()\n        }\n",
    "static dispatch",
)
text = replace_exact(
    text,
    "        ReceiverKind::Module => resolve_module_member(call, ctx),\n",
    "        ReceiverKind::Module => {\n            crate::typed_calls::resolve_module_member_outcome(call, ctx).into_legacy_result()\n        }\n",
    "module dispatch",
)
start = text.find("fn imported_module_member_candidates(\n")
end = text.find("/// Resolves a type name to a SymbolId using evidence-backed lookups only.\n")
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"static/module legacy block anchors invalid: start={start}, end={end}")
text = text[:start] + text[end:]
calls.write_text(text)
