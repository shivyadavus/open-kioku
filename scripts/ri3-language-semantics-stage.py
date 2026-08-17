from pathlib import Path

cargo = Path("crates/open-kioku-mcp/Cargo.toml")
text = cargo.read_text()
anchor = 'open-kioku-plan = { version = "2.0.0", path = "../open-kioku-plan" }\n'
addition = 'open-kioku-resolution = { version = "2.0.0", path = "../open-kioku-resolution" }\n'
if addition not in text:
    assert text.count(anchor) == 1, "unexpected MCP Cargo dependency layout"
    text = text.replace(anchor, anchor + addition)
    cargo.write_text(text)

lib = Path("crates/open-kioku-mcp/src/lib.rs")
text = lib.read_text()
old = '''        "get_evidence_schema" => {
            let manifest = store.manifest().ok().flatten();
            let schema = open_kioku_graph::schema::current_schema_with_manifest(
                Some(store as &dyn open_kioku_storage::GraphStore),
                manifest.as_ref(),
            );
            Ok(json!(schema))
        }
'''
new = '''        "get_evidence_schema" => {
            let manifest = store.manifest().ok().flatten();
            let schema = open_kioku_graph::schema::current_schema_with_manifest(
                Some(store as &dyn open_kioku_storage::GraphStore),
                manifest.as_ref(),
            );
            let mut schema = serde_json::to_value(schema)?;
            let capabilities = [
                open_kioku_core::Language::Rust,
                open_kioku_core::Language::TypeScript,
                open_kioku_core::Language::JavaScript,
                open_kioku_core::Language::Python,
                open_kioku_core::Language::Java,
                open_kioku_core::Language::Go,
            ]
            .iter()
            .filter_map(open_kioku_resolution::semantic_capabilities_for)
            .collect::<Vec<_>>();
            let object = schema
                .as_object_mut()
                .context("evidence schema must serialize as a JSON object")?;
            object.insert(
                "relationship_semantic_capability_version".into(),
                json!(open_kioku_resolution::LANGUAGE_SEMANTIC_CAPABILITY_VERSION),
            );
            object.insert(
                "relationship_semantic_capabilities".into(),
                json!(capabilities),
            );
            Ok(schema)
        }
'''
assert text.count(old) == 1, "get_evidence_schema dispatch arm changed unexpectedly"
text = text.replace(old, new)

test_anchor = '''        assert!(result.get("optional_evidence").is_some());

        // Check arrays
'''
test_new = '''        assert!(result.get("optional_evidence").is_some());
        assert_eq!(result["relationship_semantic_capability_version"], 1);
        let semantic_capabilities = result["relationship_semantic_capabilities"]
            .as_array()
            .expect("Tier-1 relationship semantic capabilities");
        assert_eq!(semantic_capabilities.len(), 6);
        let javascript = semantic_capabilities
            .iter()
            .find(|descriptor| descriptor["language"] == "javascript")
            .expect("JavaScript semantic capability descriptor");
        assert_eq!(
            javascript["capabilities"]["types_annotation"],
            "unsupported"
        );
        let java = semantic_capabilities
            .iter()
            .find(|descriptor| descriptor["language"] == "java")
            .expect("Java semantic capability descriptor");
        assert_eq!(
            java["capabilities"]["calls_instance_member"],
            "supported_authoritative"
        );
        assert_eq!(
            java["capabilities"]["calls_dynamic_dispatch"],
            "unsupported"
        );

        // Check arrays
'''
assert text.count(test_anchor) == 1, "evidence schema test layout changed unexpectedly"
text = text.replace(test_anchor, test_new)

old_description = '"get_evidence_schema" => "Use before query_evidence_graph to learn available graph node types, edge types, and properties. This is read-only and does not query graph data.",'
new_description = '"get_evidence_schema" => "Use before query_evidence_graph to learn available graph node types, edge types, properties, and the versioned Tier-1 relationship-semantic capability matrix. This is read-only and does not query graph data.",'
assert text.count(old_description) == 1, "tool guidance changed unexpectedly"
text = text.replace(old_description, new_description)

old_tool = "(\"get_evidence_schema\", \"Retrieve the versioned schema defining the supported graph node types, edge types, and query properties available in the repository's structural evidence graph.\", json!({\"type\":\"object\",\"properties\":{}})),"
new_tool = "(\"get_evidence_schema\", \"Retrieve the versioned schema defining supported graph types, query properties, and the Tier-1 relationship-semantic capability matrix.\", json!({\"type\":\"object\",\"properties\":{}})),"
assert text.count(old_tool) == 1, "tool declaration changed unexpectedly"
text = text.replace(old_tool, new_tool)

lib.write_text(text)
