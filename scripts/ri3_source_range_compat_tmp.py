from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label} seam changed: expected 1, observed {count}")
    p.write_text(text.replace(old, new, 1))


replace_exact(
    "crates/open-kioku-impact/src/lib.rs",
    '''                range: Some(LineRange { start: 1, end: 1 }),\n                is_definition: true,\n''',
    '''                range: Some(LineRange { start: 1, end: 1 }),\n                source_range: None,\n                is_definition: true,\n''',
    "impact definition occurrence fixture",
)
replace_exact(
    "crates/open-kioku-impact/src/lib.rs",
    '''                range: Some(LineRange { start: 10, end: 10 }),\n                is_definition: false,\n''',
    '''                range: Some(LineRange { start: 10, end: 10 }),\n                source_range: None,\n                is_definition: false,\n''',
    "impact reference occurrence fixture",
)
replace_exact(
    "crates/open-kioku-storage-sqlite/src/lib.rs",
    '''                    range: Some(LineRange::single(1)),\n                    is_definition: true,\n''',
    '''                    range: Some(LineRange::single(1)),\n                    source_range: None,\n                    is_definition: true,\n''',
    "sqlite definition occurrence fixture",
)
