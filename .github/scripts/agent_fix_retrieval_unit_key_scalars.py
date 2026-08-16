from pathlib import Path

path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
old = '''    #[serde(default)]
    pub line_range: Option<LineRange>,
    #[serde(default)]
    pub symbol_id: Option<SymbolId>,
'''
new = '''    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub symbol_id: Option<SymbolId>,
'''
if text.count(old) != 1:
    raise SystemExit(f'unit key range field marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''            line_range: line_range.cloned(),
            symbol_id: symbol_id.cloned(),
'''
new = '''            line_start: line_range.map(|range| range.start),
            line_end: line_range.map(|range| range.end),
            symbol_id: symbol_id.cloned(),
'''
if text.count(old) != 1:
    raise SystemExit(f'unit key range assignment marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
