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
routing.write_text(text.replace(bad_path, good_path, 1))
