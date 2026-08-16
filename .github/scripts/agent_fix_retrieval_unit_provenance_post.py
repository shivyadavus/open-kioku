from pathlib import Path

for name in [
    'crates/open-kioku-context/src/candidates.rs',
    'crates/open-kioku-context/src/lib.rs',
]:
    path = Path(name)
    text = path.read_text()
    while 'unit_key: None,\n            unit_key: Some(' in text:
        text = text.replace(
            'unit_key: None,\n            unit_key: Some(',
            'unit_key: Some(',
        )
    while 'unit_key: None,\n                unit_key: Some(' in text:
        text = text.replace(
            'unit_key: None,\n                unit_key: Some(',
            'unit_key: Some(',
        )
    path.write_text(text)
