from pathlib import Path

path = Path("crates/open-kioku-resolution/src/type_relations.rs")
text = path.read_text()
old = "        BindingId, FileId, Language, ReceiverKind, Scope, ScopeKind, SourceRange, Symbol,\n"
new = "        BindingId, FileId, Language, Scope, ScopeKind, SourceRange, Symbol,\n"
if text.count(old) != 1:
    raise SystemExit(f"type relation test import seam changed: {text.count(old)}")
path.write_text(text.replace(old, new, 1))
