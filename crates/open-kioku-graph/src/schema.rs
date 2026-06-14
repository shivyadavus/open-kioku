use open_kioku_core::{EdgeTypeSpec, EvidenceGraphSchema, NodeTypeSpec, PropertySpec};

pub fn current_schema() -> EvidenceGraphSchema {
    EvidenceGraphSchema {
        version: "1.0.0".to_string(),
        feature_flags: vec![
            "identifiers".to_string(),
            "routes".to_string(),
            "config_keys".to_string(),
        ],
        property_specs: vec![
            PropertySpec {
                name: "file_path".to_string(),
                type_name: "string".to_string(),
                description: "Repository-relative file path".to_string(),
            },
            PropertySpec {
                name: "qualified_name".to_string(),
                type_name: "string".to_string(),
                description: "Fully qualified symbol name".to_string(),
            },
        ],
        node_types: vec![
            NodeTypeSpec {
                name: "file".to_string(),
                stable: true,
                description: "A source file in the repository".to_string(),
                required_fields: vec!["file_path".to_string()],
                optional_fields: vec![],
            },
            NodeTypeSpec {
                name: "symbol".to_string(),
                stable: true,
                description: "A code symbol like a class or function".to_string(),
                required_fields: vec!["qualified_name".to_string()],
                optional_fields: vec![],
            },
        ],
        edge_types: vec![
            EdgeTypeSpec {
                name: "CONTAINS".to_string(),
                stable: true,
                description: "A directory or package contains a file or module".to_string(),
                source_types: vec!["directory".to_string(), "package".to_string()],
                target_types: vec!["file".to_string(), "module".to_string()],
                required_evidence: vec![],
            },
            EdgeTypeSpec {
                name: "CALLS".to_string(),
                stable: true,
                description: "A function or method calls another".to_string(),
                source_types: vec!["function".to_string(), "method".to_string()],
                target_types: vec!["function".to_string(), "method".to_string()],
                required_evidence: vec!["source_file".to_string()],
            },
        ],
    }
}
