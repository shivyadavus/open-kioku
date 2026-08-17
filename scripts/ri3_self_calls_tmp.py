from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

# Collapse the old calls module to dispatch only. All structural CALLS strategies now live in
# candidate/proof-aware modules.
calls = Path("crates/open-kioku-resolution/src/calls.rs")
calls.write_text('''use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use open_kioku_core::{CallSite, ReceiverKind};

pub fn resolve_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    match call.receiver_kind {
        ReceiverKind::Self_ => {
            crate::self_calls::resolve_self_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Super => {
            crate::self_calls::resolve_super_member_outcome(call, ctx).into_legacy_result()
        }
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
''')

lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = lib.read_text()
text = replace_exact(
    text,
    "mod bare_calls;\npub mod calls;\nmod typed_calls;\n",
    "mod bare_calls;\npub mod calls;\nmod self_calls;\nmod typed_calls;\n",
    "self-call module wiring",
)
lib.write_text(text)

typed = Path("crates/open-kioku-resolution/src/typed_calls.rs")
text = typed.read_text()
old_named = '''pub(crate) fn resolve_named_type_member_outcome(
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
'''
new_named = '''pub(crate) fn resolve_named_type_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_name: &str,
) -> ResolutionOutcome {
    resolve_type_names_member_outcome(call, ctx, &[type_name.to_string()])
}

pub(crate) fn resolve_type_names_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_names: &[String],
) -> ResolutionOutcome {
    let mut type_candidates = Vec::new();
    for type_name in type_names {
        type_candidates.extend(collect_type_candidates(ctx, &call.scope_id, type_name));
    }
    normalize_symbol_ids(&mut type_candidates);
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

    let mut inherited_targets = Vec::new();
    for type_id in &type_candidates {
        inherited_targets.extend(ctx.inheritance.inherited_member_candidates(
            type_id,
            &call.callee_name,
            ctx.symbols,
        ));
    }
    normalize_symbol_ids(&mut inherited_targets);
    evaluate_inherited_targets(call, ctx, inherited_targets)
}
'''
text = replace_exact(text, old_named, new_named, "typed full inheritance candidates")
text = replace_exact(
    text,
    "fn evaluate_direct_member_targets(\n",
    "pub(crate) fn evaluate_direct_member_targets(\n",
    "direct member helper visibility",
)
text = replace_exact(
    text,
    "fn evaluate_inherited_targets(\n",
    "pub(crate) fn evaluate_inherited_targets(\n",
    "inherited helper visibility",
)
text = replace_exact(
    text,
    "fn find_members_by_name(\n",
    "pub(crate) fn find_members_by_name(\n",
    "member helper visibility",
)
typed.write_text(text)
