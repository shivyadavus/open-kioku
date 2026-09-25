//! WHERE filters against a real SQLite-backed graph, not an in-crate mock.
//!
//! The unit tests resolve fields through a mock store, so they never exercise the SQL the batched
//! `DEFINES` read issues, its chunking, or the round trip of edge evidence through the index.

use chrono::Utc;
use open_kioku_core::{
    AnalysisSemanticsState, Confidence, EdgeId, Evidence, EvidenceSourceType, FileId, GraphEdge,
    GraphEdgeType, GraphNode, GraphNodeType, IndexManifest, IndexQuality, NodeId, Repository,
    RepositoryId, SymbolId,
};
use open_kioku_graph::query::{execute_graph_query, parse_graph_query, GraphQueryOptions};
use open_kioku_storage::{GraphStore, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;

/// Symbols per generated file. Above the 900-id chunk the batched read splits on, so one scanned
/// batch crosses that boundary.
const GENERATED_SYMBOLS: usize = 950;

/// An index whose manifest records the current analysis semantics. Relationship reads refuse an
/// index without one, so a store with no manifest would fail every query here before a filter ran.
fn store() -> SqliteStore {
    let store = SqliteStore::open(":memory:").expect("in-memory index");
    store.initialize().expect("schema");
    store
        .put_manifest(&IndexManifest {
            analysis_semantics: Some(AnalysisSemanticsState::current()),
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "where-filters".into(),
                root: std::path::PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: Some(Utc::now()),
            },
            file_count: 0,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        })
        .expect("manifest");
    store
}

fn file_node(id: &str, path: &str) -> GraphNode {
    GraphNode {
        id: NodeId::new(id),
        node_type: GraphNodeType::File,
        label: path.into(),
        file_id: Some(FileId::new(id)),
        ..Default::default()
    }
}

fn symbol_node(id: &str, label: &str, file: &str) -> GraphNode {
    GraphNode {
        id: NodeId::new(id),
        node_type: GraphNodeType::Function,
        label: label.into(),
        file_id: Some(FileId::new(file)),
        symbol_id: Some(SymbolId::new(id)),
        ..Default::default()
    }
}

fn edge(
    id: &str,
    from: &str,
    to: &str,
    edge_type: GraphEdgeType,
    source: &str,
    source_type: EvidenceSourceType,
    confidence: Confidence,
) -> GraphEdge {
    GraphEdge {
        id: EdgeId::new(id),
        from: NodeId::new(from),
        to: NodeId::new(to),
        edge_type,
        evidence: Evidence {
            source: source.into(),
            source_type,
            confidence,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn defines(file: &str, symbol: &str) -> GraphEdge {
    edge(
        &format!("defines:{file}:{symbol}"),
        file,
        symbol,
        GraphEdgeType::Defines,
        "open-kioku-graph",
        EvidenceSourceType::TreeSitter,
        Confidence::Exact,
    )
}

fn rows_of(store: &SqliteStore, query: &str) -> Vec<String> {
    let ast = parse_graph_query(query).unwrap_or_else(|error| panic!("{query}: {error}"));
    // These tests check filter and batching correctness, not latency: the product default's
    // 500 ms deadline is a wall-clock limit that a loaded CI runner can exceed on the
    // 950-symbol batching fixture, so give them a deadline that only a hang would reach.
    let options = GraphQueryOptions {
        deadline_ms: 60_000,
        ..GraphQueryOptions::default()
    };
    let result = execute_graph_query(store, &ast, options)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
    let mut ids = result
        .rows
        .iter()
        .map(|row| row[0]["id"].as_str().expect("node id").to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn where_fields_resolve_against_a_sqlite_index() {
    let store = store();
    let nodes = vec![
        file_node("file:config", "src/config.rs"),
        file_node("file:app", "src/app.rs"),
        symbol_node(
            "symbol:parse_config",
            "src::config::parse_config",
            "file:config",
        ),
        symbol_node("symbol:run", "src::app::run", "file:app"),
    ];
    let edges = vec![
        defines("file:config", "symbol:parse_config"),
        defines("file:app", "symbol:run"),
        edge(
            "calls:run:parse_config",
            "symbol:run",
            "symbol:parse_config",
            GraphEdgeType::Calls,
            "open-kioku-resolution",
            EvidenceSourceType::Scip,
            Confidence::Exact,
        ),
    ];
    store.replace_graph(&nodes, &edges).unwrap();

    let defines_query = "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE";
    assert_eq!(
        rows_of(
            &store,
            &format!("{defines_query} s.file_path = 'src/config.rs' RETURN s"),
        ),
        ["symbol:parse_config"]
    );
    // The label is not a path, and must not satisfy file_path just because it looks like one.
    assert!(rows_of(
        &store,
        &format!("{defines_query} s.file_path = 'src::config::parse_config' RETURN s"),
    )
    .is_empty());
    assert_eq!(
        rows_of(
            &store,
            &format!("{defines_query} s.qualified_name = 'src::app::run' RETURN s"),
        ),
        ["symbol:run"]
    );

    // Edge evidence, read back out of the index rather than from a struct built in the test.
    let calls = "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE";
    assert_eq!(
        rows_of(
            &store,
            &format!("{calls} c.confidence >= 0.9 AND c.evidence_source_type = 'scip' RETURN b"),
        ),
        ["symbol:parse_config"]
    );
    assert!(rows_of(
        &store,
        &format!("{calls} c.evidence_source = 'open-kioku-graph' RETURN b"),
    )
    .is_empty());
    assert_eq!(
        rows_of(
            &store,
            &format!("{calls} c.evidence_source = 'open-kioku-resolution' RETURN b"),
        ),
        ["symbol:parse_config"]
    );
}

/// One scanned batch naming more than 900 symbols, so the batched read splits into two chunks and
/// every symbol must still resolve its defining file.
#[test]
fn a_file_path_filter_crosses_the_batched_read_chunk_boundary() {
    let store = store();
    let mut nodes = vec![file_node("file:generated", "src/generated.rs")];
    let mut edges = Vec::new();
    for index in 0..GENERATED_SYMBOLS {
        let id = format!("symbol:generated-{index:04}");
        nodes.push(symbol_node(
            &id,
            &format!("src::generated::item_{index:04}"),
            "file:generated",
        ));
        edges.push(defines("file:generated", &id));
    }
    store.replace_graph(&nodes, &edges).unwrap();

    let query =
        "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.file_path = 'src/generated.rs' RETURN s";
    let mut matched = rows_of(&store, &format!("{query} LIMIT 500"));
    matched.extend(rows_of(&store, &format!("{query} LIMIT 500 OFFSET 500")));
    matched.sort();
    matched.dedup();
    assert_eq!(
        matched.len(),
        GENERATED_SYMBOLS,
        "every symbol across both chunks must resolve its defining file"
    );

    // A path no file carries matches nothing, rather than falling back to a label.
    assert!(rows_of(
        &store,
        "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.file_path = 'src/absent.rs' RETURN s",
    )
    .is_empty());
}

#[test]
fn the_batched_read_returns_every_chunk_ordered_by_edge_id() {
    let store = store();
    let mut nodes = vec![file_node("file:generated", "src/generated.rs")];
    let mut edges = Vec::new();
    for index in 0..GENERATED_SYMBOLS {
        let id = format!("symbol:generated-{index:04}");
        nodes.push(symbol_node(
            &id,
            &format!("src::generated::item_{index:04}"),
            "file:generated",
        ));
        edges.push(defines("file:generated", &id));
    }
    store.replace_graph(&nodes, &edges).unwrap();

    let ids = (0..GENERATED_SYMBOLS)
        .map(|index| format!("symbol:generated-{index:04}"))
        .collect::<Vec<_>>();
    let borrowed = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let read = store
        .edges_by_type_for_nodes(GraphEdgeType::Defines, &borrowed, false)
        .unwrap();

    assert_eq!(read.len(), GENERATED_SYMBOLS);
    let returned = read.iter().map(|e| e.id.0.clone()).collect::<Vec<_>>();
    let mut sorted = returned.clone();
    sorted.sort();
    assert_eq!(
        returned, sorted,
        "edges must be ordered by id across chunk boundaries, not by chunk"
    );
}
