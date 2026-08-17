from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)


typed = Path("crates/open-kioku-resolution/src/typed_calls.rs")
text = typed.read_text()
text = replace_exact(
    text,
    "    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,\n",
    "    RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,\n",
    "typed call imports",
)
text = replace_exact(
    text,
    "    let type_candidates = collect_type_candidates(ctx, type_name);\n",
    "    let type_candidates = collect_type_candidates(ctx, &call.scope_id, type_name);\n",
    "typed type-candidate call scope",
)
text = replace_exact(
    text,
    "fn collect_type_candidates(ctx: &ResolutionContext<'_>, type_name: &str) -> Vec<SymbolId> {\n",
    "fn collect_type_candidates(\n    ctx: &ResolutionContext<'_>,\n    scope_id: &ScopeId,\n    type_name: &str,\n) -> Vec<SymbolId> {\n",
    "typed type-candidate signature",
)
old_lookup = '''    for binding in ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(&ctx.scopes.innermost_scope_for_file(ctx.file_id).unwrap_or_else(|| open_kioku_core::ScopeId::new(""))), type_name)
    {
'''
new_lookup = '''    for binding in ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(scope_id), type_name)
    {
'''
text = replace_exact(text, old_lookup, new_lookup, "typed import-scope lookup")
text = text.replace("is_type_symbol(symbol.kind.clone())", "is_type_symbol(&symbol.kind)")
text = replace_exact(
    text,
    "fn is_type_symbol(kind: SymbolKind) -> bool {\n",
    "fn is_type_symbol(kind: &SymbolKind) -> bool {\n",
    "typed kind borrowing",
)
typed.write_text(text)

lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = lib.read_text()
text = replace_exact(
    text,
    "mod bare_calls;\npub mod calls;\n",
    "mod bare_calls;\npub mod calls;\nmod typed_calls;\n",
    "typed module wiring",
)
lib.write_text(text)

calls = Path("crates/open-kioku-resolution/src/calls.rs")
text = calls.read_text()
text = replace_exact(
    text,
    "        ReceiverKind::Value => resolve_typed_receiver(call, ctx),\n",
    "        ReceiverKind::Value => {\n            crate::typed_calls::resolve_typed_receiver_outcome(call, ctx).into_legacy_result()\n        }\n",
    "typed receiver dispatch",
)
start = text.find("fn resolve_typed_receiver(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {")
end = text.find("fn resolve_module_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {")
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"typed resolver anchors invalid: start={start}, end={end}")
text = text[:start] + text[end:]
calls.write_text(text)
