from pathlib import Path

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = '''            caveats: vec!["semantic index is stale".into()],
            traces: Vec::new(),
            selection: Default::default(),
        };
'''
new = '''            caveats: vec!["semantic index is stale".into()],
            traces: Vec::new(),
            selection: Default::default(),
            routing: Default::default(),
        };
'''
if text.count(old) != 1:
    raise SystemExit(f'compact retrieval diagnostics fixture marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))
