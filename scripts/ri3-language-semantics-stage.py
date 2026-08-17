from pathlib import Path

# Expose the existing versioned capability contract through MCP diagnostics.
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
            .find(|descriptor| descriptor["language"] == "java_script")
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

# Make Tree-sitter receiver classification consume the shared language-semantics layer rather
# than maintaining a second, subtly divergent classifier.
ts_cargo = Path("crates/open-kioku-tree-sitter/Cargo.toml")
text = ts_cargo.read_text()
anchor = 'open-kioku-errors = { version = "2.0.0", path = "../open-kioku-errors" }\n'
addition = 'open-kioku-languages = { version = "2.0.0", path = "../open-kioku-languages" }\n'
if addition not in text:
    assert text.count(anchor) == 1, "unexpected Tree-sitter Cargo dependency layout"
    text = text.replace(anchor, anchor + addition)
    ts_cargo.write_text(text)

ts = Path("crates/open-kioku-tree-sitter/src/lib.rs")
text = ts.read_text()
assert text.count('receiver_kind = classify_receiver_string(&recv);') == 5, "receiver classifier call sites changed unexpectedly"
text = text.replace(
    'receiver_kind = classify_receiver_string(&recv);',
    'receiver_kind = classify_receiver_string(&file.language, &recv);',
)
old_rust_scoped = '''                    if let Some(path_node) = function_node.child_by_field_name("path") {
                        let recv = path_node.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_receiver_string(&file.language, &recv);
                            receiver_text = Some(recv);
                        }
                    }
'''
new_rust_scoped = '''                    if let Some(path_node) = function_node.child_by_field_name("path") {
                        let recv = path_node.utf8_text(source_bytes).unwrap_or("").to_string();
                        if !recv.is_empty() {
                            receiver_kind = classify_rust_path_receiver(&recv);
                            receiver_text = Some(recv);
                        }
                    }
'''
assert text.count(old_rust_scoped) == 1, "Rust scoped-call extraction changed unexpectedly"
text = text.replace(old_rust_scoped, new_rust_scoped)
old_classifier = '''fn classify_receiver_string(recv: &str) -> ReceiverKind {
    let recv = recv.trim();
    if recv == "this"
        || recv == "self"
        || recv == "Self"
        || recv.starts_with("this.")
        || recv.starts_with("self.")
        || recv.starts_with("Self::")
    {
        ReceiverKind::Self_
    } else if recv == "super"
        || recv == "Super"
        || recv.starts_with("super.")
        || recv.starts_with("Super::")
    {
        ReceiverKind::Super
    } else if recv
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && !recv.contains('.')
    {
        ReceiverKind::Type
    } else if recv.starts_with("crate::") {
        ReceiverKind::Module
    } else {
        ReceiverKind::Value
    }
}
'''
new_classifier = '''fn classify_receiver_string(language: &Language, recv: &str) -> ReceiverKind {
    open_kioku_languages::semantics_for(language)
        .map(|semantics| semantics.classify_receiver(recv))
        .unwrap_or(ReceiverKind::Value)
}

fn classify_rust_path_receiver(recv: &str) -> ReceiverKind {
    let recv = recv.trim();
    if matches!(recv, "crate" | "self" | "super")
        || recv.starts_with("crate::")
        || recv.starts_with("self::")
        || recv.starts_with("super::")
    {
        ReceiverKind::Module
    } else {
        classify_receiver_string(&Language::Rust, recv)
    }
}
'''
assert text.count(old_classifier) == 1, "receiver classifier implementation changed unexpectedly"
text = text.replace(old_classifier, new_classifier)

test_anchor = '''    fn does_not_emit_json_keys_as_symbols() {
'''
receiver_test = '''    #[test]
    fn rust_scoped_paths_distinguish_modules_from_instance_self() {
        let file = File {
            id: FileId::new("file_rust_paths"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "fn run() { crate::target(); self::target(); super::target(); Self::target(); self.target(); }",
        )
        .expect("Rust qualified-call fixture should parse");
        let kinds = facts
            .calls
            .iter()
            .filter_map(|call| call.receiver.as_deref().map(|receiver| (receiver, &call.receiver_kind)))
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(kinds.get("crate").copied(), Some(&ReceiverKind::Module));
        assert_eq!(kinds.get("self").copied(), Some(&ReceiverKind::Module));
        assert_eq!(kinds.get("super").copied(), Some(&ReceiverKind::Module));
        assert_eq!(kinds.get("Self").copied(), Some(&ReceiverKind::Self_));
        assert!(facts.calls.iter().any(|call| {
            call.receiver.as_deref() == Some("self")
                && call.receiver_kind == ReceiverKind::Self_
                && call.callee_name == "target"
        }));
    }

    #[test]
'''
# The BTreeMap would collapse self::target and self.target because both receiver text is "self".
# Keep the path assertions separate by source range/call order instead of relying on the map.
receiver_test = '''    #[test]
    fn rust_scoped_paths_distinguish_modules_from_instance_self() {
        let file = File {
            id: FileId::new("file_rust_paths"),
            repository_id: RepositoryId::new("repo"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "fn run() { crate::crate_target(); self::self_target(); super::super_target(); Self::type_target(); self.instance_target(); }",
        )
        .expect("Rust qualified-call fixture should parse");
        let kind_for = |callee: &str| {
            facts
                .calls
                .iter()
                .find(|call| call.callee_name == callee)
                .map(|call| (call.receiver.as_deref(), call.receiver_kind.clone()))
                .expect("qualified call")
        };

        assert_eq!(kind_for("crate_target"), (Some("crate"), ReceiverKind::Module));
        assert_eq!(kind_for("self_target"), (Some("self"), ReceiverKind::Module));
        assert_eq!(kind_for("super_target"), (Some("super"), ReceiverKind::Module));
        assert_eq!(kind_for("type_target"), (Some("Self"), ReceiverKind::Self_));
        assert_eq!(kind_for("instance_target"), (Some("self"), ReceiverKind::Self_));
    }

    #[test]
'''
assert text.count(test_anchor) == 1, "Tree-sitter tests anchor changed unexpectedly"
text = text.replace(test_anchor, receiver_test + '    fn does_not_emit_json_keys_as_symbols() {\n')
ts.write_text(text)

# Extend exact Rust path resolution to direct crate/self/super roots while preserving ambiguity.
typed = Path("crates/open-kioku-resolution/src/typed_calls.rs")
text = typed.read_text()
old_helper = '''    let module = receiver.strip_prefix("crate::")?;
    if module.is_empty() {
        return None;
    }

    let mut targets = Vec::new();
    for qualified_name in rust_qualified_module_symbol_names(module, &call.callee_name) {
        if let Some(ids) = ctx.symbols.by_qualified.get(&qualified_name) {
            targets.extend(ids.iter().cloned());
        }
    }
'''
new_helper = '''    let qualified_names =
        rust_qualified_module_symbol_names(ctx.file_path, receiver, &call.callee_name)?;

    let mut targets = Vec::new();
    for qualified_name in qualified_names {
        if let Some(ids) = ctx.symbols.by_qualified.get(&qualified_name) {
            targets.extend(ids.iter().cloned());
        }
    }
'''
assert text.count(old_helper) == 1, "Rust qualified module resolver changed unexpectedly"
text = text.replace(old_helper, new_helper)
old_names = '''fn rust_qualified_module_symbol_names(module: &str, callee: &str) -> [String; 2] {
    [
        format!("src::{module}::{callee}"),
        format!("src::{module}::mod::{callee}"),
    ]
}
'''
new_names = '''fn rust_qualified_module_symbol_names(
    file_path: &std::path::Path,
    receiver: &str,
    callee: &str,
) -> Option<Vec<String>> {
    let receiver = receiver.trim();
    let stem = file_path
        .with_extension("")
        .to_string_lossy()
        .replace(['/', '\\\\'], "::");
    let logical_module = rust_logical_module_prefix(&stem);

    let mut names = match receiver {
        "crate" => rust_crate_root_member_names(&stem, callee),
        "self" => vec![format!("{stem}::{callee}")],
        "super" => rust_super_member_names(&logical_module, callee),
        _ if receiver.starts_with("crate::") => {
            let module = receiver.trim_start_matches("crate::");
            rust_module_member_names(&format!("src::{module}"), callee)
        }
        _ if receiver.starts_with("self::") => {
            let module = receiver.trim_start_matches("self::");
            rust_module_member_names(&format!("{logical_module}::{module}"), callee)
        }
        _ if receiver.starts_with("super::") => {
            let module = receiver.trim_start_matches("super::");
            let parent = rust_parent_module_prefix(&logical_module)?;
            rust_module_member_names(&format!("{parent}::{module}"), callee)
        }
        _ => return None,
    };
    names.sort();
    names.dedup();
    Some(names)
}

fn rust_logical_module_prefix(stem: &str) -> String {
    if let Some(prefix) = stem.strip_suffix("::mod") {
        prefix.to_string()
    } else if let Some(prefix) = stem.strip_suffix("::lib") {
        prefix.to_string()
    } else if let Some(prefix) = stem.strip_suffix("::main") {
        prefix.to_string()
    } else {
        stem.to_string()
    }
}

fn rust_parent_module_prefix(module: &str) -> Option<String> {
    module.rsplit_once("::").map(|(parent, _)| parent.to_string())
}

fn rust_crate_root_member_names(stem: &str, callee: &str) -> Vec<String> {
    if stem.ends_with("::lib") || stem.ends_with("::main") {
        vec![format!("{stem}::{callee}")]
    } else {
        let root = stem.split("::").next().unwrap_or("src");
        vec![
            format!("{root}::lib::{callee}"),
            format!("{root}::main::{callee}"),
        ]
    }
}

fn rust_super_member_names(logical_module: &str, callee: &str) -> Vec<String> {
    let Some(parent) = rust_parent_module_prefix(logical_module) else {
        return Vec::new();
    };
    if !parent.contains("::") {
        vec![
            format!("{parent}::lib::{callee}"),
            format!("{parent}::main::{callee}"),
        ]
    } else {
        rust_module_member_names(&parent, callee)
    }
}

fn rust_module_member_names(module: &str, callee: &str) -> Vec<String> {
    vec![
        format!("{module}::{callee}"),
        format!("{module}::mod::{callee}"),
    ]
}
'''
assert text.count(old_names) == 1, "Rust qualified module name helper changed unexpectedly"
text = text.replace(old_names, new_names)
old_test = '''    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        assert_eq!(
            rust_qualified_module_symbol_names("storage", "persist"),
            [
                "src::storage::persist".to_string(),
                "src::storage::mod::persist".to_string(),
            ]
        );
    }
'''
new_test = '''    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        assert_eq!(
            rust_qualified_module_symbol_names(
                std::path::Path::new("src/lib.rs"),
                "crate::storage",
                "persist"
            )
            .unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_qualified_module_symbol_names(
                std::path::Path::new("src/storage/service.rs"),
                "self",
                "persist"
            )
            .unwrap(),
            vec!["src::storage::service::persist".to_string()]
        );
        assert_eq!(
            rust_qualified_module_symbol_names(
                std::path::Path::new("src/storage/service.rs"),
                "super",
                "persist"
            )
            .unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
    }
'''
assert text.count(old_test) == 1, "Rust module-name test changed unexpectedly"
text = text.replace(old_test, new_test)
typed.write_text(text)
