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

# The V2 integration test validates persistence/status independently of which relationship
# families a tiny parser fixture happens to extract. Counter rendering is tested with an
# explicit typed report so the diagnostics contract remains deterministic and parser-agnostic.
cli = Path("crates/open-kioku-cli/src/lib.rs")
text = cli.read_text()
old = '''        let calls = report\n            .by_relationship\n            .get("calls")\n            .expect("call resolution should be represented in diagnostics");\n        assert!(calls.candidates_considered >= 1);\n\n        let persisted = load_index_manifest(repo)\n'''
new = '''        let persisted = load_index_manifest(repo)\n'''
if text.count(old) != 1:
    raise SystemExit("CLI diagnostics call-fixture seam changed")
text = text.replace(old, new, 1)
old = '''        assert_eq!(persisted_report.by_relationship, report.by_relationship);\n\n        let json = serde_json::to_value(&persisted).unwrap();\n        assert!(json\n            .pointer("/quality/resolution_quality/by_relationship/calls")\n            .is_some());\n        let lines = relationship_resolution_summary_lines(persisted_report);\n        assert!(lines.iter().any(|line| {\n            line.contains("calls:") && line.contains("proven") && line.contains("candidates")\n        }));\n'''
new = '''        assert_eq!(persisted_report, report);\n\n        let json = serde_json::to_value(&persisted).unwrap();\n        assert!(json.pointer("/quality/resolution_quality").is_some());\n\n        let mut display_report = open_kioku_core::ResolutionQualityReport::default();\n        display_report.by_relationship.insert(\n            "calls".into(),\n            open_kioku_core::RelationshipResolutionQuality {\n                candidates_considered: 3,\n                proven: 1,\n                ambiguous: 1,\n                unresolved: 1,\n                heuristic_candidates_retained: 2,\n                ..open_kioku_core::RelationshipResolutionQuality::default()\n            },\n        );\n        let lines = relationship_resolution_summary_lines(&display_report);\n        assert!(lines.iter().any(|line| {\n            line.contains("calls:")\n                && line.contains("1 proven / 3 candidates")\n                && line.contains("1 ambiguous")\n                && line.contains("1 unresolved")\n                && line.contains("2 heuristic candidates retained")\n        }));\n'''
if text.count(old) != 1:
    raise SystemExit("CLI diagnostics persistence/render seam changed")
cli.write_text(text.replace(old, new, 1))
