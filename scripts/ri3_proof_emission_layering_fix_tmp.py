from pathlib import Path

path = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = path.read_text()
block = '''    proof.details.insert(
        "start_column".into(),
        serde_json::Value::from(call.range.start_column),
    );
    proof.details.insert(
        "end_column".into(),
        serde_json::Value::from(call.range.end_column),
    );
'''
if text.count(block) != 1:
    raise SystemExit(f"proof column detail seam changed: {text.count(block)}")
path.write_text(text.replace(block, "", 1))
