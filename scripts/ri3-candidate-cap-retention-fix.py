from pathlib import Path

path = Path("crates/open-kioku-resolution/src/pipeline.rs")
text = path.read_text()
old = '''    if candidates_considered > MAX_RESOLUTION_CANDIDATES {
        return ResolutionOutcome::Ambiguous {
            candidates,
            reason: format!(
                "candidate cap hit: {candidates_considered} unique structural candidates exceed the safe maximum {MAX_RESOLUTION_CANDIDATES}; authoritative emission suppressed"
            ),
            candidates_considered,
            candidate_cap_hit: true,
        };
    }
'''
new = '''    if candidates_considered > MAX_RESOLUTION_CANDIDATES {
        let retained_candidates = candidates
            .into_iter()
            .take(MAX_RESOLUTION_CANDIDATES)
            .collect();
        return ResolutionOutcome::Ambiguous {
            candidates: retained_candidates,
            reason: format!(
                "candidate cap hit: {candidates_considered} unique structural candidates exceed the safe maximum {MAX_RESOLUTION_CANDIDATES}; authoritative emission suppressed and retained diagnostics bounded"
            ),
            candidates_considered,
            candidate_cap_hit: true,
        };
    }
'''
assert text.count(old) == 1, "candidate cap branch changed unexpectedly"
text = text.replace(old, new)
text = text.replace(
    'assert_eq!(candidates.len(), MAX_RESOLUTION_CANDIDATES + 1);\n            assert!(reason.contains("authoritative emission suppressed"));',
    'assert_eq!(candidates.len(), MAX_RESOLUTION_CANDIDATES);\n            assert!(reason.contains("authoritative emission suppressed"));\n            assert!(reason.contains("retained diagnostics bounded"));'
)
path.write_text(text)
