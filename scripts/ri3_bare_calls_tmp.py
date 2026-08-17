from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)


lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = lib.read_text()
text = replace_exact(text, "pub mod calls;\n", "mod bare_calls;\npub mod calls;\n", "bare module")
lib.write_text(text)

calls = Path("crates/open-kioku-resolution/src/calls.rs")
text = calls.read_text()
text = replace_exact(
    text,
    "    CallSite, Confidence, EvidenceSourceType, FileRange, Language, LineRange, ReceiverKind,\n    SymbolId, SymbolKind,\n",
    "    CallSite, Confidence, EvidenceSourceType, FileRange, LineRange, ReceiverKind, SymbolId,\n    SymbolKind,\n",
    "calls import",
)
text = replace_exact(
    text,
    "        ReceiverKind::None => resolve_bare_call(call, ctx),\n",
    "        ReceiverKind::None => crate::bare_calls::resolve_bare_call_outcome(call, ctx).into_legacy_result(),\n",
    "bare dispatch",
)
start = text.find("fn resolve_bare_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {")
end = text.find("fn resolve_module_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {")
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"bare call function anchors invalid: start={start}, end={end}")
text = text[:start] + text[end:]
calls.write_text(text)

bare_calls = Path("crates/open-kioku-resolution/src/bare_calls.rs")
text = bare_calls.read_text()
text = replace_exact(
    text,
    "    proof.ambiguity = ambiguity.to_vec();\n",
    "    proof.ambiguity = ambiguity.iter().map(|id| id.0.clone()).collect();\n",
    "relationship proof ambiguity serialization",
)
bare_calls.write_text(text)

relationship = Path("crates/open-kioku-core/src/relationship.rs")
text = relationship.read_text()
old = '''        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope)))
        }
'''
new = '''        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || same_scope
                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope)))
        }
'''
text = replace_exact(text, old, new, "CALLS same-scope authority")
relationship.write_text(text)
