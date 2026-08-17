from pathlib import Path

# Preserve compatibility for existing explicit IndexQuality constructors.
ingest = Path("crates/open-kioku-ingest/src/lib.rs")
text = ingest.read_text()
old = '''            semantic_provider_notes,\n            quality_notes,\n        }\n'''
new = '''            semantic_provider_notes,\n            resolution_quality: None,\n            quality_notes,\n        }\n'''
observed = text.count(old)
if observed != 2:
    raise SystemExit(f"IndexQuality constructor seam changed: expected 2, observed {observed}")
ingest.write_text(text.replace(old, new, 2))

# Keep the permanent core regression strict-Clippy clean.
core = Path("crates/open-kioku-core/src/lib.rs")
text = core.read_text()
old = '''        let mut quality = IndexQuality::default();\n        quality.resolution_quality = Some(report.clone());\n'''
new = '''        let quality = IndexQuality {\n            resolution_quality: Some(report.clone()),\n            ..IndexQuality::default()\n        };\n'''
observed = text.count(old)
if observed != 1:
    raise SystemExit(f"core diagnostics test seam changed: expected 1, observed {observed}")
core.write_text(text.replace(old, new, 1))
