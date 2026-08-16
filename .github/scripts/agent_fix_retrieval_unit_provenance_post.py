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
    if name.endswith('/lib.rs'):
        text = text.replace(
            '{} selected retrieval unit(s) lack unambiguous retrieval-trace source attribution',
            '{} selected retrieval unit(s) lack retrieval-trace source attribution because unit identity is ambiguous or unavailable',
        )
    path.write_text(text)
