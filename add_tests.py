import re

with open('crates/open-kioku-graph/src/query.rs', 'r') as f:
    content = f.read()

tests = """
    use open_kioku_storage::GraphStore;
    use open_kioku_storage_sqlite::SqliteStore;
    use open_kioku_storage::schema::{GraphNodeType, GraphEdgeType};
    use open_kioku_storage::RecordMeta;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn setup_test_store() -> SqliteStore {
        let dir = tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("test.db")).unwrap();
        store.setup_schema().unwrap();

        let meta = RecordMeta {
            timestamp: chrono::Utc::now().timestamp(),
            index_version: "1.0".into(),
        };

        // Add some test nodes
        store.write_node(
            GraphNodeType::File,
            "file1",
            "file1.rs",
            "src/file1.rs",
            meta.clone(),
            None,
        ).unwrap();

        store.write_node(
            GraphNodeType::Function,
            "func1",
            "my_func",
            "src/file1.rs",
            meta.clone(),
            None,
        ).unwrap();

        store.write_node(
            GraphNodeType::Function,
            "func2",
            "other_func",
            "src/file2.rs",
            meta.clone(),
            None,
        ).unwrap();

        store.write_edge(
            GraphEdgeType::Defines,
            "file1",
            "func1",
            1.0,
            meta.clone(),
            None,
        ).unwrap();

        store.write_edge(
            GraphEdgeType::Calls,
            "func1",
            "func2",
            1.0,
            meta.clone(),
            None,
        ).unwrap();

        store
    }

    #[test]
    fn test_execution_one_hop_respects_edge_type() {
        let store = setup_test_store();
        let ast = parse_graph_query("MATCH (f:File)-[:DEFINES]->(fn:Function) RETURN f, fn").unwrap();
        let options = GraphQueryOptions::default();
        let result = execute_graph_query(&store, &ast, options).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("f").unwrap().get("id").unwrap().as_str().unwrap(), "file1");
        assert_eq!(result.rows[0].get("fn").unwrap().get("id").unwrap().as_str().unwrap(), "func1");

        // Fails with wrong edge
        let ast2 = parse_graph_query("MATCH (f:File)-[:CALLS]->(fn:Function) RETURN f, fn").unwrap();
        let result2 = execute_graph_query(&store, &ast2, GraphQueryOptions::default()).unwrap();
        assert_eq!(result2.rows.len(), 0);
    }

    #[test]
    fn test_execution_multi_hop_respects_edge_type() {
        let store = setup_test_store();
        let ast = parse_graph_query("MATCH (f:File)-[:DEFINES *1..2]->(x) RETURN x").unwrap();
        let options = GraphQueryOptions::default();
        let result = execute_graph_query(&store, &ast, options).unwrap();

        // Should only match func1 (DEFINES), but NOT func2 because func1 -> func2 is CALLS
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("x").unwrap().get("id").unwrap().as_str().unwrap(), "func1");
    }

    #[test]
    fn test_execution_property_filter() {
        let store = setup_test_store();
        let ast = parse_graph_query("MATCH (f:File)-[:DEFINES]->(fn:Function) WHERE fn.qualified_name = 'my_func' RETURN fn").unwrap();
        let options = GraphQueryOptions::default();
        let result = execute_graph_query(&store, &ast, options).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("fn").unwrap().get("label").unwrap().as_str().unwrap(), "my_func");

        let ast2 = parse_graph_query("MATCH (f:File)-[:DEFINES]->(fn:Function) WHERE fn.qualified_name = 'wrong' RETURN fn").unwrap();
        let result2 = execute_graph_query(&store, &ast2, GraphQueryOptions::default()).unwrap();
        assert_eq!(result2.rows.len(), 0);
    }

    #[test]
    fn test_timeout_returns_structured_error() {
        let store = setup_test_store();
        let ast = parse_graph_query("MATCH (f:File)-[:DEFINES *1..5]->(fn:Function) RETURN f, fn").unwrap();
        let mut options = GraphQueryOptions::default();
        options.deadline_ms = 0; // Immediate timeout
        let result = execute_graph_query(&store, &ast, options);
        assert!(matches!(result.unwrap_err(), GraphQueryError::Timeout));
    }
}
"""

content = content.replace("}\n}\n", "}\n" + tests + "\n")

with open('crates/open-kioku-graph/src/query.rs', 'w') as f:
    f.write(content)
