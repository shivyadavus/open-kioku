from pathlib import Path

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = '''        let candidate_limit = limit
            .saturating_mul(routing.policy.candidate_multiplier)
            .clamp(20, 200);
'''
new = '''        let candidate_limit = routing.policy.request_limit(limit).clamp(20, 200);
'''
if text.count(old) != 1:
    raise SystemExit(f'candidate request limit marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        streams.retain(|stream| routing.policy.allows(stream.source));
        streams.extend(external_streams);
        // Task routing changes which evidence families run and how much candidate headroom they
'''
new = '''        streams.retain(|stream| routing.policy.allows(stream.source));
        streams.extend(external_streams);
        for stream in &mut streams {
            stream
                .candidates
                .truncate(routing.policy.candidate_cap(stream.source, limit));
        }
        // Task routing changes which evidence families run and how much candidate headroom they
'''
if text.count(old) != 1:
    raise SystemExit(f'per-stream cap marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            if !contributed {
                diagnostics.caveats.push(format!(
                    "task-family policy requires {} evidence, but it did not contribute task-relevant evidence",
                    retrieval_source_label(*required)
                ));
            }
'''
new = '''            if !contributed {
                let requirement = if routing.policy.missing_required_evidence_is_blocker {
                    "blocking requirement"
                } else {
                    "required evidence"
                };
                diagnostics.caveats.push(format!(
                    "task-family {requirement}: {} did not contribute task-relevant evidence",
                    retrieval_source_label(*required)
                ));
            }
'''
if text.count(old) != 1:
    raise SystemExit(f'required evidence blocker marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
