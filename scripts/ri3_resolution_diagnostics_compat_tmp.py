from pathlib import Path

path = Path("crates/open-kioku-ingest/src/lib.rs")
text = path.read_text()
old = '''            semantic_provider_notes,\n            quality_notes,\n        }\n'''
new = '''            semantic_provider_notes,\n            resolution_quality: None,\n            quality_notes,\n        }\n'''
observed = text.count(old)
if observed != 2:
    raise SystemExit(f"IndexQuality constructor seam changed: expected 2, observed {observed}")
path.write_text(text.replace(old, new, 2))
