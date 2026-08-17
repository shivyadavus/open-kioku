from pathlib import Path

path = Path("crates/open-kioku-ingest/src/lib.rs")
text = path.read_text()
old = '''        let mut quality = open_kioku_core::IndexQuality::default();
        let mut report = open_kioku_core::ResolutionQualityReport::default();
        report.candidate_cap_hits = 2;
        attach_resolution_quality(&mut quality, Some(report));
'''
new = '''        let mut quality = open_kioku_core::IndexQuality::default();
        let report = open_kioku_core::ResolutionQualityReport {
            candidate_cap_hits: 2,
            ..Default::default()
        };
        attach_resolution_quality(&mut quality, Some(report));
'''
assert text.count(old) == 1, "candidate cap quality test changed unexpectedly"
path.write_text(text.replace(old, new))
