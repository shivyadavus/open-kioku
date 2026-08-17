from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

# Fix the conservative outer-type simple-name extraction without relying on unsupported multi-pattern rsplit.
types = Path("crates/open-kioku-resolution/src/type_candidates.rs")
text = types.read_text()
old = '''    let simple_name = type_name
        .rsplit(["::", "."])
        .next()
        .unwrap_or(type_name.as_str());
'''
new = '''    let simple_name = type_name
        .rsplit_once("::")
        .map(|(_, name)| name)
        .or_else(|| type_name.rsplit_once('.').map(|(_, name)| name))
        .unwrap_or(type_name.as_str());
'''
text = replace_exact(text, old, new, "outer type simple-name extraction")
types.write_text(text)

calls = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = calls.read_text()
old = '''use crate::pipeline::{
    evaluate_candidates, ResolutionCandidate, ResolutionOutcome, ResolutionStrategy,
};
'''
new = '''use crate::pipeline::{
    evaluate_candidates, ResolutionCandidate, ResolutionOutcome, ResolutionStrategy,
};
use crate::type_candidates::{discover_type_candidates, TypeDiscovery};
'''
text = replace_exact(text, old, new, "shared type candidate import")
old = '''use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TypeDiscovery {
    SameFile,
    ImportBinding,
    QualifiedName,
}

#[derive(Debug, Clone)]
struct TypeCandidate {
    target: SymbolId,
    discoveries: Vec<TypeDiscovery>,
}
'''
new = '''use std::collections::BTreeMap;
'''
text = replace_exact(text, old, new, "private type candidate types")
old = '''    let type_candidates = discover_type_candidates(ctx, type_name);
'''
new = '''    let type_candidates = discover_type_candidates(
        ctx.file_id,
        Some(&call.scope_id),
        type_name,
        ctx.repository,
        ctx.symbols,
    );
'''
text = replace_exact(text, old, new, "typed call shared discovery")
start = text.find('fn discover_type_candidates(ctx: &ResolutionContext<\'_>, type_name: &str) -> Vec<TypeCandidate> {')
end = text.find('fn members_by_name(', start)
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"private type discovery block not found: start={start}, end={end}")
text = text[:start] + text[end:]
start = text.find('fn is_named_type(ctx: &ResolutionContext<\'_>, target: &SymbolId, name: &str) -> bool {')
end = text.find('struct CandidateTemplate', start)
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"private type helper block not found: start={start}, end={end}")
text = text[:start] + text[end:]
calls.write_text(text)

lib = Path("crates/open-kioku-resolution/src/lib.rs")
text = lib.read_text()
old = '''pub mod pipeline;

pub use call_candidates::resolve_call_outcome;
'''
new = '''pub mod pipeline;
pub mod type_candidates;

pub use call_candidates::resolve_call_outcome;
'''
text = replace_exact(text, old, new, "type candidate module export")
old = '''pub use pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
    ResolutionStrategy,
};
'''
new = '''pub use pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
    ResolutionStrategy,
};
pub use type_candidates::{
    discover_type_candidates, normalize_outer_type_name, TypeCandidate, TypeDiscovery,
};
'''
text = replace_exact(text, old, new, "type candidate API export")
lib.write_text(text)
