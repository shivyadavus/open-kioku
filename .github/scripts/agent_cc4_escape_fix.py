from pathlib import Path

bs = chr(92)

for name in [
    "crates/open-kioku-context/src/routing.rs",
    "crates/open-kioku-semantic/src/lib.rs",
]:
    path = Path(name)
    text = path.read_text()
    bad = "replace('" + bs + "', \"/\")"
    good = "replace('" + (bs * 2) + "', \"/\")"
    if bad not in text:
        raise SystemExit(f"{name}: missing generated backslash replacement marker")
    text = text.replace(bad, good)
    path.write_text(text)

routing = Path("crates/open-kioku-context/src/routing.rs")
text = routing.read_text()
bad_path = "crates" + bs + "open-kioku-context" + bs + "src"
good_path = "crates" + (bs * 2) + "open-kioku-context" + (bs * 2) + "src"
if bad_path not in text:
    raise SystemExit("routing.rs: missing generated Windows path test marker")
text = text.replace(bad_path, good_path, 1)

old = '''        if single_path_lookup
            || previous
                .as_deref()
                .is_some_and(is_explicit_path_scope_marker)
        {
            if !scope.contains(&path) {
                scope.push(path);
            }
        }
'''
new = '''        if (single_path_lookup
            || previous
                .as_deref()
                .is_some_and(is_explicit_path_scope_marker))
            && !scope.contains(&path)
        {
            scope.push(path);
        }
'''
if text.count(old) != 1:
    raise SystemExit(f"routing.rs: collapsible-if marker count={text.count(old)}")
text = text.replace(old, new, 1)

old = '''    loop {
        let Some((prefix, suffix)) = token.rsplit_once(':') else {
            break;
        };
        if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty() {
            token = prefix.to_string();
        } else {
            break;
        }
    }
'''
new = '''    while let Some((prefix, suffix)) = token.rsplit_once(':') {
        if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty() {
            token = prefix.to_string();
        } else {
            break;
        }
    }
'''
if text.count(old) != 1:
    raise SystemExit(f"routing.rs: while-let marker count={text.count(old)}")
text = text.replace(old, new, 1)
routing.write_text(text)
