use open_kioku_core::{
    EdgeTypeSpec, EvidenceGraphSchema, IndexManifest, NodeTypeSpec, OptionalEvidenceSpec,
    PropertySpec,
};

pub fn current_schema(store: Option<&dyn open_kioku_storage::GraphStore>) -> EvidenceGraphSchema {
    current_schema_with_manifest(store, None)
}

pub fn current_schema_with_manifest(
    store: Option<&dyn open_kioku_storage::GraphStore>,
    manifest: Option<&IndexManifest>,
) -> EvidenceGraphSchema {
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
        "Resource",
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
        "TestCovers",
        "Validates",
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
            "service_boundaries".to_string(),
            "read_only_graph_query".to_string(),
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
            PropertySpec {
                name: "protocol".to_string(),
                type_name: "string".to_string(),
                description: "Service-boundary protocol such as http, tcp, graphql, grpc, or trpc"
                    .to_string(),
            },
            PropertySpec {
                name: "normalized_path".to_string(),
                type_name: "string".to_string(),
                description:
                    "Normalized route, URL path, port, topic, queue, config key, or resource target"
                        .to_string(),
            },
            PropertySpec {
                name: "source_framework".to_string(),
                type_name: "string".to_string(),
                description:
                    "Static/runtime pass or framework that produced a service-boundary fact"
                        .to_string(),
            },
        ],
        node_types,
        edge_types,
        evidence_source_types: evidence_source_types(),
        query_features: query_features(),
        optional_evidence: optional_evidence(manifest),
        caveats: schema_caveats(manifest),
        indexed_at: manifest.map(|m| m.indexed_at.to_rfc3339()),
    }
}

fn evidence_source_types() -> Vec<String> {
    [
        "tree_sitter",
        "scip",
        "lsp",
        "regex",
        "lexical",
        "semantic",
        "runtime",
        "git_history",
        "static_analysis",
        "external_integration",
        "heuristic",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn query_features() -> Vec<String> {
    [
        "node_type_filters",
        "edge_type_filters",
        "one_hop_directed_traversal",
        "bounded_multi_hop_traversal",
        "property_equality_filters",
        "property_prefix_filters",
        "regex_filters_on_label_qualified_name_file_path",
        "return_variables",
        "limit",
        "offset",
        "row_cap",
        "depth_cap",
        "timeout",
        "read_only_syntax_rejection",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn optional_evidence(manifest: Option<&IndexManifest>) -> Vec<OptionalEvidenceSpec> {
    let Some(manifest) = manifest else {
        return [
            ("scip", "SCIP exact reference evidence"),
            ("semantic", "local semantic vector index evidence"),
            ("runtime", "runtime trace and incident evidence"),
            ("history", "local git history and co-change evidence"),
            ("coverage", "coverage report evidence"),
            ("junit", "JUnit-style test report evidence"),
            ("architecture_policy", "architecture policy graph evidence"),
        ]
        .into_iter()
        .map(|(name, description)| OptionalEvidenceSpec {
            name: name.to_string(),
            available: false,
            status: "unknown".to_string(),
            evidence_count: 0,
            description: description.to_string(),
            caveats: vec![
                "index manifest is unavailable; run `ok index .` for availability".into(),
            ],
        })
        .collect();
    };

    let quality = &manifest.quality;
    vec![
        optional_spec(
            "scip",
            quality.scip_exact_references,
            "SCIP exact reference evidence",
        ),
        optional_spec(
            "semantic",
            quality.semantic_provider_notes.len(),
            "local semantic vector index evidence",
        ),
        optional_spec(
            "runtime",
            quality.runtime_analysis_facts,
            "runtime trace and incident evidence",
        ),
        optional_spec(
            "history",
            quality.git_history_facts,
            "local git history and co-change evidence",
        ),
        optional_spec(
            "coverage",
            quality.coverage_reports,
            "coverage report evidence",
        ),
        optional_spec(
            "junit",
            quality.junit_reports,
            "JUnit-style test report evidence",
        ),
        optional_spec(
            "architecture_policy",
            quality.architecture_facts,
            "architecture policy graph evidence",
        ),
    ]
}

fn optional_spec(name: &str, count: usize, description: &str) -> OptionalEvidenceSpec {
    let available = count > 0;
    OptionalEvidenceSpec {
        name: name.to_string(),
        available,
        status: if available {
            "available"
        } else {
            "not_observed"
        }
        .to_string(),
        evidence_count: count,
        description: description.to_string(),
        caveats: if available {
            Vec::new()
        } else {
            vec!["no persisted evidence for this family is present in the current index".into()]
        },
    }
}

fn schema_caveats(manifest: Option<&IndexManifest>) -> Vec<String> {
    if manifest.is_some() {
        vec!["schema availability reflects the current persisted index manifest".into()]
    } else {
        vec!["schema type vocabulary is available, but repository evidence counts require an index manifest".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use open_kioku_core::{IndexQuality, Repository, RepositoryId};
    use std::path::PathBuf;

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
        assert_eq!(schema1.node_types.len(), 23);
        assert_eq!(schema1.edge_types.len(), 25);

        // Ensure count properties are absent in JSON (since they are None and skip_serializing_if is used)
        assert!(!json1.contains("\"count\":"));
        assert!(schema1
            .evidence_source_types
            .contains(&"git_history".to_string()));
        assert!(schema1
            .query_features
            .contains(&"bounded_multi_hop_traversal".to_string()));
        assert!(schema1
            .optional_evidence
            .iter()
            .all(|evidence| evidence.status == "unknown"));
        assert!(schema1.indexed_at.is_none());
    }

    #[test]
    fn test_schema_uses_manifest_for_optional_evidence_availability() {
        let indexed_at = Utc::now();
        let manifest = IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: Some(indexed_at),
            },
            file_count: 1,
            symbol_count: 1,
            chunk_count: 1,
            indexed_at,
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality {
                scip_exact_references: 2,
                runtime_analysis_facts: 3,
                git_history_facts: 4,
                coverage_reports: 1,
                ..Default::default()
            },
        };

        let schema = current_schema_with_manifest(None, Some(&manifest));
        let expected_indexed_at = indexed_at.to_rfc3339();
        assert_eq!(
            schema.indexed_at.as_deref(),
            Some(expected_indexed_at.as_str())
        );

        let scip = schema
            .optional_evidence
            .iter()
            .find(|evidence| evidence.name == "scip")
            .unwrap();
        assert!(scip.available);
        assert_eq!(scip.status, "available");
        assert_eq!(scip.evidence_count, 2);

        let runtime = schema
            .optional_evidence
            .iter()
            .find(|evidence| evidence.name == "runtime")
            .unwrap();
        assert!(runtime.available);
        assert_eq!(runtime.evidence_count, 3);

        let junit = schema
            .optional_evidence
            .iter()
            .find(|evidence| evidence.name == "junit")
            .unwrap();
        assert!(!junit.available);
        assert_eq!(junit.status, "not_observed");
        assert!(!junit.caveats.is_empty());
    }
}
