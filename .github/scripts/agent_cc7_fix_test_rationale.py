from pathlib import Path

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
replacements = [
    ('evidence_refs: Vec::new(),\n                    },', 'evidence_refs: Vec::new(),\n                        rationale: "lexical fixture".into(),\n                    },', 2),
    ('evidence_refs: vec!["symbol:a".into()],\n                    },', 'evidence_refs: vec!["symbol:a".into()],\n                        rationale: "exact semantic fixture".into(),\n                    },', 1),
]
for old, new, expected in replacements:
    count = text.count(old)
    if count < expected:
        raise SystemExit(f'rationale fixture marker count={count}, expected at least {expected}: {old!r}')
    text = text.replace(old, new, expected)
path.write_text(text)
