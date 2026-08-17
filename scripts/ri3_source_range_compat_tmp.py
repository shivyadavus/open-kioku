from pathlib import Path

path = Path("crates/open-kioku-impact/src/lib.rs")
text = path.read_text()
replacements = [
    (
        '''                range: Some(LineRange { start: 1, end: 1 }),\n                is_definition: true,\n''',
        '''                range: Some(LineRange { start: 1, end: 1 }),\n                source_range: None,\n                is_definition: true,\n''',
        "definition occurrence fixture",
    ),
    (
        '''                range: Some(LineRange { start: 10, end: 10 }),\n                is_definition: false,\n''',
        '''                range: Some(LineRange { start: 10, end: 10 }),\n                source_range: None,\n                is_definition: false,\n''',
        "reference occurrence fixture",
    ),
]
for old, new, label in replacements:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label} seam changed: expected 1, observed {count}")
    text = text.replace(old, new, 1)
path.write_text(text)
