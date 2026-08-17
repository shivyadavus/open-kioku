from pathlib import Path

path = Path("crates/open-kioku-graph/src/lib.rs")
text = path.read_text()
old = '''        let caller = make_symbol("caller", "src/service", "run");
        let callee = make_symbol("callee", "src/service", "save");
        let relationships = vec![
'''
new = '''        let caller = make_symbol("caller", "src/service", "run");
        let callee = make_symbol("callee", "src/service", "save");
        let callee_id = callee.id.clone();
        let relationships = vec![
'''
if text.count(old) != 1:
    raise SystemExit(f"callee identity seam changed: expected 1, observed {text.count(old)}")
text = text.replace(old, new, 1)
old_assert = '''            .all(|proof| proof.target_symbol_id.as_ref() == Some(&callee.id)));
'''
new_assert = '''            .all(|proof| proof.target_symbol_id.as_ref() == Some(&callee_id)));
'''
if text.count(old_assert) != 1:
    raise SystemExit(
        f"callee proof assertion seam changed: expected 1, observed {text.count(old_assert)}"
    )
path.write_text(text.replace(old_assert, new_assert, 1))
