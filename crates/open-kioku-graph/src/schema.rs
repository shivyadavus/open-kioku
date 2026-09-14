use crate::query::{DEFAULT_MAX_DEPTH, HARD_MAX_DEPTH, HARD_ROW_LIMIT};
use open_kioku_core::{
    EdgeTypeSpec, EvidenceGraphSchema, GraphEdgeType, GraphNodeType, GraphQueryExample,
    IndexManifest, NodeTypeSpec, OptionalEvidenceSpec, PropertySpec, UnsupportedGraphQueryForm,
};

/// Node types in schema order. The schema advertises these and the query parser resolves and lists
/// types from them, so the two cannot disagree. A new `GraphNodeType` variant fails to compile
/// in `node_type_name`; add it here as well.
pub(crate) const NODE_TYPES: [GraphNodeType; 23] = [
    GraphNodeType::File,
    GraphNodeType::Directory,
    GraphNodeType::Module,
    GraphNodeType::Package,
    GraphNodeType::Class,
    GraphNodeType::Trait,
    GraphNodeType::Interface,
    GraphNodeType::Function,
    GraphNodeType::Method,
    GraphNodeType::Field,
    GraphNodeType::Endpoint,
    GraphNodeType::DatabaseTable,
    GraphNodeType::Collection,
    GraphNodeType::Queue,
    GraphNodeType::Topic,
    GraphNodeType::ConfigKey,
    GraphNodeType::Test,
    GraphNodeType::BuildTarget,
    GraphNodeType::RuntimeError,
    GraphNodeType::Ticket,
    GraphNodeType::PullRequest,
    GraphNodeType::Resource,
    GraphNodeType::ArchitectureComponent,
];

/// Edge types in schema order, shared with the parser the same way as `NODE_TYPES`.
pub(crate) const EDGE_TYPES: [GraphEdgeType; 29] = [
    GraphEdgeType::Contains,
    GraphEdgeType::Defines,
    GraphEdgeType::References,
    GraphEdgeType::UsesType,
    GraphEdgeType::Calls,
    GraphEdgeType::Implements,
    GraphEdgeType::Extends,
    GraphEdgeType::Imports,
    GraphEdgeType::DependsOn,
    GraphEdgeType::ExposesEndpoint,
    GraphEdgeType::CallsEndpoint,
    GraphEdgeType::ReadsConfig,
    GraphEdgeType::WritesConfig,
    GraphEdgeType::ReadsTable,
    GraphEdgeType::WritesTable,
    GraphEdgeType::PublishesEvent,
    GraphEdgeType::ConsumesEvent,
    GraphEdgeType::Tests,
    GraphEdgeType::TestCovers,
    GraphEdgeType::Validates,
    GraphEdgeType::OwnedBy,
    GraphEdgeType::ChangedBy,
    GraphEdgeType::FailedIn,
    GraphEdgeType::BelongsTo,
    GraphEdgeType::MentionedIn,
    GraphEdgeType::RelatedToTicket,
    GraphEdgeType::SimilarTo,
    GraphEdgeType::SemanticallyRelated,
    GraphEdgeType::DerivedFrom,
];

/// The schema name, which is also the key graph type statistics are stored under.
pub(crate) fn node_type_name(node_type: &GraphNodeType) -> &'static str {
    match node_type {
        GraphNodeType::File => "File",
        GraphNodeType::Directory => "Directory",
        GraphNodeType::Module => "Module",
        GraphNodeType::Package => "Package",
        GraphNodeType::Class => "Class",
        GraphNodeType::Trait => "Trait",
        GraphNodeType::Interface => "Interface",
        GraphNodeType::Function => "Function",
        GraphNodeType::Method => "Method",
        GraphNodeType::Field => "Field",
        GraphNodeType::Endpoint => "Endpoint",
        GraphNodeType::DatabaseTable => "DatabaseTable",
        GraphNodeType::Collection => "Collection",
        GraphNodeType::Queue => "Queue",
        GraphNodeType::Topic => "Topic",
        GraphNodeType::ConfigKey => "ConfigKey",
        GraphNodeType::Test => "Test",
        GraphNodeType::BuildTarget => "BuildTarget",
        GraphNodeType::RuntimeError => "RuntimeError",
        GraphNodeType::Ticket => "Ticket",
        GraphNodeType::PullRequest => "PullRequest",
        GraphNodeType::Resource => "Resource",
        GraphNodeType::ArchitectureComponent => "ArchitectureComponent",
    }
}

pub(crate) fn edge_type_name(edge_type: &GraphEdgeType) -> &'static str {
    match edge_type {
        GraphEdgeType::Contains => "Contains",
        GraphEdgeType::Defines => "Defines",
        GraphEdgeType::References => "References",
        GraphEdgeType::UsesType => "UsesType",
        GraphEdgeType::Calls => "Calls",
        GraphEdgeType::Implements => "Implements",
        GraphEdgeType::Extends => "Extends",
        GraphEdgeType::Imports => "Imports",
        GraphEdgeType::DependsOn => "DependsOn",
        GraphEdgeType::ExposesEndpoint => "ExposesEndpoint",
        GraphEdgeType::CallsEndpoint => "CallsEndpoint",
        GraphEdgeType::ReadsConfig => "ReadsConfig",
        GraphEdgeType::WritesConfig => "WritesConfig",
        GraphEdgeType::ReadsTable => "ReadsTable",
        GraphEdgeType::WritesTable => "WritesTable",
        GraphEdgeType::PublishesEvent => "PublishesEvent",
        GraphEdgeType::ConsumesEvent => "ConsumesEvent",
        GraphEdgeType::Tests => "Tests",
        GraphEdgeType::TestCovers => "TestCovers",
        GraphEdgeType::Validates => "Validates",
        GraphEdgeType::OwnedBy => "OwnedBy",
        GraphEdgeType::ChangedBy => "ChangedBy",
        GraphEdgeType::FailedIn => "FailedIn",
        GraphEdgeType::BelongsTo => "BelongsTo",
        GraphEdgeType::MentionedIn => "MentionedIn",
        GraphEdgeType::RelatedToTicket => "RelatedToTicket",
        GraphEdgeType::SimilarTo => "SimilarTo",
        GraphEdgeType::SemanticallyRelated => "SemanticallyRelated",
        GraphEdgeType::DerivedFrom => "DerivedFrom",
    }
}

/// The serialized snake_case spelling: `database_table` for the schema's `DatabaseTable`.
pub(crate) fn node_type_query_spelling(node_type: &GraphNodeType) -> String {
    underscored(node_type_name(node_type))
}

/// Edge types serialize as SCREAMING_SNAKE_CASE: the schema's `DependsOn` is `DEPENDS_ON`.
pub(crate) fn edge_type_query_spelling(edge_type: &GraphEdgeType) -> String {
    underscored(edge_type_name(edge_type)).to_ascii_uppercase()
}

/// Resolves a node type named in a query. The schema's spelling and the serialized one are both
/// accepted, case-insensitively, so every name the schema advertises parses and queries written
/// against the serialized names keep working.
pub(crate) fn node_type_for_query_name(name: &str) -> Option<GraphNodeType> {
    NODE_TYPES
        .iter()
        .find(|node_type| {
            name.eq_ignore_ascii_case(node_type_name(node_type))
                || name.eq_ignore_ascii_case(&node_type_query_spelling(node_type))
        })
        .cloned()
}

/// Resolves an edge type named in a query, accepting `DependsOn` and `DEPENDS_ON` alike.
pub(crate) fn edge_type_for_query_name(name: &str) -> Option<GraphEdgeType> {
    EDGE_TYPES
        .iter()
        .find(|edge_type| {
            name.eq_ignore_ascii_case(edge_type_name(edge_type))
                || name.eq_ignore_ascii_case(&edge_type_query_spelling(edge_type))
        })
        .cloned()
}

fn underscored(name: &str) -> String {
    let mut spelling = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            spelling.push('_');
        }
        spelling.push(character.to_ascii_lowercase());
    }
    spelling
}

pub fn current_schema(store: Option<&dyn open_kioku_storage::GraphStore>) -> EvidenceGraphSchema {
    current_schema_with_manifest(store, None)
}

pub fn current_schema_with_manifest(
    store: Option<&dyn open_kioku_storage::GraphStore>,
    manifest: Option<&IndexManifest>,
) -> EvidenceGraphSchema {
    let node_stats = store.and_then(|s| s.node_type_stats().ok());
    let edge_stats = store.and_then(|s| s.edge_type_stats().ok());

    let mut node_types = Vec::new();
    for node_type in &NODE_TYPES {
        let name = node_type_name(node_type);
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
    for edge_type in &EDGE_TYPES {
        let name = edge_type_name(edge_type);
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
            "relationship_proofs".to_string(),
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
        syntax: query_syntax(),
        examples: query_examples(),
        unsupported: unsupported_query_forms(),
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

// Every sentence here describes `query.rs` as it behaves; the parser tests run `examples` and
// reject every `unsupported` entry, so a grammar change that makes this stale fails there.
fn query_syntax() -> Vec<String> {
    vec![
        "A query is MATCH <path> [WHERE <filter> [AND <filter>]...] RETURN <variable>[, <variable>]..., optionally followed by LIMIT <n> and OFFSET <n> in either order; keywords are case-insensitive.".into(),
        "A path is exactly one edge pattern between two nodes, such as (f:File)-[:DEFINES]->(s:Function) or (s:Function)<-[:DEFINES]-(f:File); a MATCH without an edge pattern is rejected.".into(),
        "A node is (variable:Type); the variable and the :Type are each optional, so (f), (:File) and () are nodes.".into(),
        "A one-hop edge is -[:TYPE]-> or <-[:TYPE]-, and it must name its type to run.".into(),
        format!("A multi-hop edge is -[:TYPE *min..max]-> with 1 <= min <= max, where max may not exceed the depth cap ({DEFAULT_MAX_DEPTH} unless raised, never above {HARD_MAX_DEPTH}); the :TYPE is optional, the source node must name its type, and only forward edges are followed."),
        "Type names are case-insensitive and may be written as node_types and edge_types name them or in their underscored form: (t:DatabaseTable) or (t:database_table), [:DependsOn] or [:DEPENDS_ON].".into(),
        "A filter is variable.field = 'text', variable.field STARTS_WITH 'text', or variable.field =~ 'regex' on a node variable bound in MATCH, with a single- or double-quoted value.".into(),
        "Filter fields are label, id, file_path, qualified_name, source, source_type and confidence; file_path and qualified_name compare against the node label, and graph nodes carry no source, source_type or confidence field, so filters on those match no rows.".into(),
        "=~ applies to label, file_path and qualified_name only, with a valid regex of at most 100 bytes.".into(),
        "RETURN lists node variables bound in MATCH, each at most once; read labels and properties from the returned node objects.".into(),
        format!("LIMIT is clamped to {HARD_ROW_LIMIT} rows, and a write-like or composition keyword (CREATE, MERGE, DELETE, DETACH, SET, REMOVE, DROP, CALL, LOAD, UNION, WITH, FOREACH) rejects the whole query."),
    ]
}

fn query_examples() -> Vec<GraphQueryExample> {
    [
        (
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 10",
            "Functions and the files that define them.",
        ),
        (
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.file_path STARTS_WITH 'src/' RETURN s",
            "Functions defined in files under src/; file_path compares against the File node label, which is its path.",
        ),
        (
            "MATCH (caller:Function)-[:CALLS]->(callee:Function) WHERE callee.label = 'parse_config' RETURN caller",
            "Direct callers of functions named parse_config.",
        ),
        (
            "MATCH (s:Function)<-[:DEFINES]-(f:File) WHERE s.label =~ '^handle_' RETURN s, f",
            "The DEFINES edge read in reverse, filtered by a label regex.",
        ),
        (
            "MATCH (a:Function)-[:CALLS *1..3]->(b:Function) WHERE a.label = 'main' RETURN b LIMIT 20",
            "Functions reachable from main within one to three CALLS hops.",
        ),
        (
            "MATCH (f:File)-[:IMPORTS]->(g:File) WHERE g.file_path = 'src/config.rs' RETURN f",
            "Files whose imports resolve to src/config.rs.",
        ),
    ]
    .into_iter()
    .map(|(query, description)| GraphQueryExample {
        query: query.to_string(),
        description: description.to_string(),
    })
    .collect()
}

fn unsupported_query_forms() -> Vec<UnsupportedGraphQueryForm> {
    vec![
        unsupported(
            "isolated_node",
            "MATCH (f:File) RETURN f",
            "Match the node through an edge pattern, such as MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f; a node with no edges cannot be matched.",
        ),
        unsupported(
            "property_access_in_return",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f.file_path",
            "RETURN f and read its label or properties from the returned node; filter properties in WHERE.",
        ),
        unsupported(
            "return_expressions",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN count(s)",
            "RETURN node variables only; aggregates, DISTINCT, AS aliases and * are not parsed.",
        ),
        unsupported(
            "clauses_after_return",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN s ORDER BY s",
            "Only LIMIT <n> and OFFSET <n> may follow RETURN; there is no ordering clause, and OFFSET replaces SKIP.",
        ),
        unsupported(
            "more_than_one_edge_pattern",
            "MATCH (f:File)-[:DEFINES]->(a:Function)-[:CALLS]->(b:Function) RETURN f, b",
            "Run one query per edge, or use a multi-hop range when every hop has the same edge type.",
        ),
        unsupported(
            "edge_variable",
            "MATCH (a:Function)-[c:CALLS]->(b:Function) RETURN a, b",
            "Omit the edge variable and write -[:CALLS]->; edges cannot be bound, filtered or returned.",
        ),
        unsupported(
            "undirected_edge",
            "MATCH (a:Function)-[:CALLS]-(b:Function) RETURN a, b",
            "Give the edge a direction with -[:CALLS]-> or <-[:CALLS]-, one query per direction.",
        ),
        unsupported(
            "one_hop_edge_without_type",
            "MATCH (a:Function)-[]->(b) RETURN a, b",
            "Name the edge type, such as -[:CALLS]->.",
        ),
        unsupported(
            "reverse_multi_hop",
            "MATCH (b:Function)<-[:CALLS *1..2]-(a:Function) RETURN a, b",
            "Write the path forward: MATCH (a:Function)-[:CALLS *1..2]->(b:Function) RETURN a, b.",
        ),
        unsupported(
            "unbounded_variable_length_edge",
            "MATCH (a:Function)-[:CALLS *]->(b:Function) RETURN b",
            "Give an explicit hop range, such as -[:CALLS *1..3]->.",
        ),
        unsupported(
            "hop_range_above_depth_cap",
            "MATCH (a:Function)-[:CALLS *1..6]->(b:Function) RETURN b",
            format!("Keep max hops within the depth cap: {DEFAULT_MAX_DEPTH} unless raised, never above {HARD_MAX_DEPTH}."),
        ),
        unsupported(
            "multi_hop_without_source_type",
            "MATCH (a)-[:CALLS *1..2]->(b:Function) RETURN b",
            "Give the source node a type, such as (a:Function)-[:CALLS *1..2]->(b).",
        ),
        unsupported(
            "or_or_not_in_where",
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.label = 'main' OR s.label = 'run' RETURN s",
            "Combine filters with AND only; for alternatives on label, file_path or qualified_name use =~ '^(main|run)$'.",
        ),
        unsupported(
            "other_filter_operators",
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.label CONTAINS 'parse' RETURN s",
            "Use =, STARTS_WITH or =~; =~ 'parse' matches a substring of label, file_path or qualified_name.",
        ),
        unsupported(
            "unsupported_filter_field",
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.protocol = 'http' RETURN f",
            "Filter on label, id, file_path or qualified_name, and read other properties from the returned nodes.",
        ),
        unsupported(
            "inline_property_map",
            "MATCH (f:File {label: 'src/main.rs'})-[:DEFINES]->(s:Function) RETURN s",
            "Move the condition to WHERE: WHERE f.label = 'src/main.rs'.",
        ),
        unsupported(
            "write_or_composition_keyword",
            "MATCH (f:File)-[:DEFINES]->(s:Function) DETACH DELETE s",
            "The query language is read-only; CREATE, MERGE, DELETE, DETACH, SET, REMOVE, DROP, CALL, LOAD, UNION, WITH and FOREACH reject the query.",
        ),
    ]
}

fn unsupported(
    form: &str,
    example: &str,
    alternative: impl Into<String>,
) -> UnsupportedGraphQueryForm {
    UnsupportedGraphQueryForm {
        form: form.to_string(),
        example: example.to_string(),
        alternative: alternative.into(),
    }
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
        assert_eq!(schema1.edge_types.len(), 29);
        assert!(schema1
            .edge_types
            .iter()
            .any(|edge| edge.name == "UsesType"));
        assert!(schema1
            .edge_types
            .iter()
            .any(|edge| edge.name == "DerivedFrom"));
        assert!(schema1
            .feature_flags
            .contains(&"relationship_proofs".to_string()));

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
    fn schema_describes_the_query_language_with_examples_and_rejected_forms() {
        let schema = current_schema(None);

        assert!(!schema.syntax.is_empty());
        assert!(schema.examples.len() >= 4);
        assert!(schema
            .examples
            .iter()
            .any(|example| example.query.contains(" WHERE ") && example.query.contains('.')));
        assert!(schema
            .examples
            .iter()
            .any(|example| example.query.contains(" *")));
        for form in [
            "isolated_node",
            "property_access_in_return",
            "reverse_multi_hop",
        ] {
            assert!(
                schema.unsupported.iter().any(|entry| entry.form == form),
                "unsupported forms must list {form}"
            );
        }
    }

    #[test]
    fn query_spellings_use_the_serialized_type_names() {
        assert_eq!(
            node_type_query_spelling(&GraphNodeType::DatabaseTable),
            "database_table"
        );
        assert_eq!(node_type_query_spelling(&GraphNodeType::File), "file");
        assert_eq!(
            edge_type_query_spelling(&GraphEdgeType::DependsOn),
            "DEPENDS_ON"
        );
        for node_type in &NODE_TYPES {
            assert_eq!(
                serde_json::to_value(node_type).unwrap(),
                node_type_query_spelling(node_type)
            );
        }
        for edge_type in &EDGE_TYPES {
            assert_eq!(
                serde_json::to_value(edge_type).unwrap(),
                edge_type_query_spelling(edge_type)
            );
        }
    }

    #[test]
    fn test_schema_uses_manifest_for_optional_evidence_availability() {
        let indexed_at = Utc::now();
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
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
