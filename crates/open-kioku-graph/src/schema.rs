use open_kioku_core::{EdgeTypeSpec, EvidenceGraphSchema, NodeTypeSpec, PropertySpec};

pub fn current_schema(store: Option<&dyn open_kioku_storage::GraphStore>) -> EvidenceGraphSchema {
    let node_variants = vec![
        "File",
        "Directory",
        "Module",
        "Package",
        "Class",
        "Trait",
        "Interface",
        "Function",
        "Method",
        "Field",
        "Endpoint",
        "DatabaseTable",
        "Collection",
        "Queue",
        "Topic",
        "ConfigKey",
        "Test",
        "BuildTarget",
        "RuntimeError",
        "Ticket",
        "PullRequest",
        "ArchitectureComponent",
    ];

    let edge_variants = vec![
        "Contains",
        "Defines",
        "References",
        "Calls",
        "Implements",
        "Extends",
        "Imports",
        "DependsOn",
        "ExposesEndpoint",
        "CallsEndpoint",
        "ReadsConfig",
        "WritesConfig",
        "ReadsTable",
        "WritesTable",
        "PublishesEvent",
        "ConsumesEvent",
        "Tests",
        "OwnedBy",
        "ChangedBy",
        "FailedIn",
        "BelongsTo",
        "MentionedIn",
        "RelatedToTicket",
    ];

    let node_stats = store.and_then(|s| s.node_type_stats().ok());
    let edge_stats = store.and_then(|s| s.edge_type_stats().ok());

    let mut node_types = Vec::new();
    for name in node_variants {
        let mut count = None;
        let mut evidence_available = None;
        let mut freshness = None;

        if let Some(stats) = &node_stats {
            if let Some(s) = stats.get(name) {
                count = Some(s.count);
                evidence_available = Some(s.evidence_available);
                freshness = s.freshness.map(|v| v.to_string());
            } else {
                count = Some(0);
                evidence_available = Some(false);
            }
        }

        node_types.push(NodeTypeSpec {
            name: name.to_string(),
            stable: true,
            description: format!("Node of type {}", name),
            required_fields: vec![],
            optional_fields: vec![],
            count,
            evidence_available,
            freshness,
        });
    }

    let mut edge_types = Vec::new();
    for name in edge_variants {
        let mut count = None;
        let mut evidence_available = None;
        let mut freshness = None;

        if let Some(stats) = &edge_stats {
            if let Some(s) = stats.get(name) {
                count = Some(s.count);
                evidence_available = Some(s.evidence_available);
                freshness = s.freshness.map(|v| v.to_string());
            } else {
                count = Some(0);
                evidence_available = Some(false);
            }
        }

        edge_types.push(EdgeTypeSpec {
            name: name.to_string(),
            stable: true,
            description: format!("Edge of type {}", name),
            source_types: vec![],
            target_types: vec![],
            required_evidence: vec![],
            count,
            evidence_available,
            freshness,
        });
    }

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
        node_types,
        edge_types,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_json_deterministic() {
        let schema1 = current_schema(None);
        let schema2 = current_schema(None);

        let json1 = serde_json::to_string(&schema1).unwrap();
        let json2 = serde_json::to_string(&schema2).unwrap();

        assert_eq!(
            json1, json2,
            "Schema JSON serialization must be deterministic"
        );

        // Verify node types has the correct counts
        assert_eq!(schema1.node_types.len(), 22);
        assert_eq!(schema1.edge_types.len(), 23);

        // Ensure count properties are absent in JSON (since they are None and skip_serializing_if is used)
        assert!(!json1.contains("\"count\":"));
    }
}
