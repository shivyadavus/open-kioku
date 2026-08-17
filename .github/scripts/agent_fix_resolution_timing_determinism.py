from pathlib import Path

# Branch-local validator: keep the MCP tool-list golden contract in lock-step with the
# intentional get_evidence_schema description change. Do not alter semantic-resolution timing:
# #239 explicitly requires enrichment-time telemetry, and no deterministic product failure has
# demonstrated that it needs to be removed from the durable report.
path = Path('crates/open-kioku-tests/snapshots/tools_list.json')
text = path.read_text()
old = '''Retrieve the versioned schema defining the supported graph node types, edge types, and query properties available in the repository's structural evidence graph. Use before query_evidence_graph to learn available graph node types, edge types, and properties. This is read-only and does not query graph data.'''
new = '''Retrieve the versioned schema defining supported graph types, query properties, and the Tier-1 relationship-semantic capability matrix. Use before query_evidence_graph to learn available graph node types, edge types, properties, and the versioned Tier-1 relationship-semantic capability matrix. This is read-only and does not query graph data.'''
if text.count(old) != 1:
    raise SystemExit(f'tools_list get_evidence_schema snapshot marker count={text.count(old)}')
path.write_text(text.replace(old, new, 1))
