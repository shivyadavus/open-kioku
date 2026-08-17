from pathlib import Path

calls = Path("crates/open-kioku-resolution/src/calls.rs")
text = calls.read_text()
old = '''        ReceiverKind::Unknown => ResolutionOutcome::Unresolved {
            candidates: Vec::new(),
            reason: "unsupported dynamic/unknown receiver cannot be proven structurally".into(),
        },
'''
new = '''        ReceiverKind::Unknown => ResolutionOutcome::Unresolved {
            candidates: Vec::new(),
            reason: "unsupported dynamic/unknown receiver cannot be proven structurally".into(),
            candidates_considered: 0,
        },
'''
assert text.count(old) == 1, "unknown receiver outcome changed unexpectedly"
calls.write_text(text.replace(old, new))
