from pathlib import Path

path = Path('crates/open-kioku-context/src/candidates.rs')
text = path.read_text()
old = '''            sources_attempted: attempted.into_iter().collect(),
            sources_succeeded: succeeded.into_iter().collect(),
        },
'''
new = '''            sources_attempted: attempted.into_iter().collect(),
            sources_succeeded: succeeded.into_iter().collect(),
            selection: Default::default(),
        },
'''
if text.count(old) != 1:
    raise SystemExit(f'candidate diagnostics repair count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = '''    order.extend((0..ranked.len()).filter(|index| !order.contains(index)));
'''
new = '''    let exact_indices = order.iter().copied().collect::<std::collections::BTreeSet<_>>();
    order.extend((0..ranked.len()).filter(|index| !exact_indices.contains(index)));
'''
if text.count(old) != 1:
    raise SystemExit(f'order borrow repair count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            caveats: vec!["semantic index is stale".into()],
            traces: Vec::new(),
        };
'''
new = '''            caveats: vec!["semantic index is stale".into()],
            traces: Vec::new(),
            selection: Default::default(),
        };
'''
if text.count(old) != 1:
    raise SystemExit(f'context diagnostics fixture repair count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
