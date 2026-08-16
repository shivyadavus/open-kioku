from pathlib import Path

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = 'ContextBudget, ContextPack, Evidence'
new = 'ContextBudget, ContextPack, ContextSelectedUnit, Evidence'
if text.count(old) != 1:
    raise SystemExit(f'ContextSelectedUnit import marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))
