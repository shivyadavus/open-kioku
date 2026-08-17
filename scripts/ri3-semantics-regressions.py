#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PATH = ROOT / "crates/open-kioku-mcp/src/lib.rs"
text = PATH.read_text()

# MCP golden fixtures represent a freshly indexed repository, not a legacy index.
old = '''    fn fixture_manifest() -> IndexManifest {
        serde_json::from_value(json!({
            "repository": {
                "id": "repo",
                "name": "mcp-fixture",
                "root": ".",
                "branch": "main",
                "commit": "abc123",
                "indexed_at": "2026-01-01T00:00:00Z"
            },
            "file_count": 2,
            "symbol_count": 2,
            "chunk_count": 1,
            "indexed_at": "2026-01-01T00:00:00Z",
            "schema_version": 1,
            "index_mode": "full",
            "phase_reports": []
        }))
        .unwrap()
    }
'''
new = '''    fn fixture_manifest() -> IndexManifest {
        let mut manifest: IndexManifest = serde_json::from_value(json!({
            "repository": {
                "id": "repo",
                "name": "mcp-fixture",
                "root": ".",
                "branch": "main",
                "commit": "abc123",
                "indexed_at": "2026-01-01T00:00:00Z"
            },
            "file_count": 2,
            "symbol_count": 2,
            "chunk_count": 1,
            "indexed_at": "2026-01-01T00:00:00Z",
            "schema_version": 1,
            "index_mode": "full",
            "phase_reports": []
        }))
        .unwrap();
        manifest.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::current());
        manifest
    }
'''
if text.count(old) != 1:
    raise SystemExit(f"fixture_manifest patch count={text.count(old)}")
text = text.replace(old, new, 1)

# The graph-pagination test also needs a current-generation manifest before reading graph truth.
old = '''        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let root = GraphNode {
'''
new = '''        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let manifest = fixture_manifest();
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[],
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        let root = GraphNode {
'''
# Only patch the occurrence inside query_evidence_graph_returns_metadata_and_continuation.
anchor = text.find("async fn query_evidence_graph_returns_metadata_and_continuation()")
if anchor < 0:
    raise SystemExit("graph pagination test missing")
tail = text[anchor:]
if old not in tail:
    raise SystemExit("graph pagination manifest insertion point missing")
tail = tail.replace(old, new, 1)
text = text[:anchor] + tail

# Persisted IMPLEMENTS facts are relationship truth too; do not expose them under stale semantics.
old = '''        "get_implementations" => implementation_lookup_tool(store, &params),
'''
new = '''        "get_implementations" => {
            require_authoritative_relationships(store)?;
            implementation_lookup_tool(store, &params)
        }
'''
if old not in text:
    raise SystemExit("get_implementations dispatch point missing")
text = text.replace(old, new, 1)

# Adversarial consumer regression: diagnostics remain available, relationship reads fail closed,
# and non-authoritative repository reads remain usable on a legacy index.
marker = '''    #[tokio::test]
    async fn query_evidence_graph_returns_metadata_and_continuation() {
'''
test = '''    #[tokio::test]
    async fn legacy_analysis_semantics_are_reported_and_relationship_reads_fail_closed() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let mut manifest = fixture_manifest();
        manifest.analysis_semantics = None;
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[],
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let status = dispatch(Path::new("."), &store, &config, "repo_status", json!({}))
            .await
            .unwrap();
        assert_eq!(
            status["analysis_semantics_status"]["status"],
            "rebuild_required"
        );
        assert!(status["analysis_semantics_status"]["recommended_action"]
            .as_str()
            .unwrap()
            .contains("ok index"));

        let graph = dispatch(
            Path::new("."),
            &store,
            &config,
            "query_evidence_graph",
            json!({"query": "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 1"}),
        )
        .await
        .unwrap_err();
        let message = graph.to_string();
        assert!(message.contains("authoritative relationship evidence unavailable"));
        assert!(message.contains("RebuildRequired"));

        let files = dispatch(Path::new("."), &store, &config, "list_files", json!({}))
            .await
            .unwrap();
        assert_eq!(files["returned"], 0);
    }

'''
if test not in text:
    if marker not in text:
        raise SystemExit("MCP regression insertion point missing")
    text = text.replace(marker, test + marker, 1)

PATH.write_text(text)
print("RI3 semantic consumer regressions staged")
