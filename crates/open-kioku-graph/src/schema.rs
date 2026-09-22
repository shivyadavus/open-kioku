use crate::query::{DEFAULT_MAX_DEPTH, HARD_MAX_DEPTH, HARD_ROW_LIMIT};
use open_kioku_core::{
    Confidence, EdgeTypeSpec, EvidenceGraphSchema, GraphEdgeType, GraphNodeType, GraphQueryExample,
    IndexManifest, NodeTypeSpec, OptionalEvidenceSpec, PropertySpec, UnsupportedGraphQueryForm,
};

/// Node types in schema order. The schema advertises these and the query parser resolves and lists
/// types from them, so the two cannot disagree. The compiler does not tie this list to
/// `GraphNodeType` (only `node_type_name`'s match is exhaustive); `type_lists_hold_every_variant`
/// fails when a variant is missing.
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

/// The node types the graph builder gives a symbol (`symbol_node_type` in lib.rs). A node of one
/// of these types built from an indexed symbol carries a `symbol_id`; one built from an import or
/// an analysis fact does not. `symbol_node_types_are_the_types_the_builder_gives_symbols` ties
/// this list to the builder.
pub(crate) const SYMBOL_NODE_TYPES: [GraphNodeType; 10] = [
    GraphNodeType::Module,
    GraphNodeType::Class,
    GraphNodeType::Trait,
    GraphNodeType::Interface,
    GraphNodeType::Function,
    GraphNodeType::Method,
    GraphNodeType::Field,
    GraphNodeType::Endpoint,
    GraphNodeType::DatabaseTable,
    GraphNodeType::Test,
];

// WHERE fields per binding. The parser validates filters against these and the schema's syntax
// sentence is built from them, so an agent is never told a field exists that the parser rejects.
const FILE_FILTER_FIELDS: &[&str] = &["label", "id", "file_path"];
const SYMBOL_FILTER_FIELDS: &[&str] = &["label", "id", "file_path", "qualified_name"];
const OTHER_NODE_FILTER_FIELDS: &[&str] = &["label", "id"];
/// Graph nodes carry no evidence; these are read from a bound edge's `Evidence`.
pub(crate) const EDGE_FILTER_FIELDS: &[&str] = &["source", "source_type", "confidence"];

/// The WHERE fields a node variable takes. An untyped node may bind a File or a symbol node, so it
/// takes every node field and each row resolves the field for the node it holds.
pub(crate) fn node_filter_fields(node_type: Option<&GraphNodeType>) -> &'static [&'static str] {
    match node_type {
        Some(GraphNodeType::File) => FILE_FILTER_FIELDS,
        Some(node_type) if SYMBOL_NODE_TYPES.contains(node_type) => SYMBOL_FILTER_FIELDS,
        Some(_) => OTHER_NODE_FILTER_FIELDS,
        None => SYMBOL_FILTER_FIELDS,
    }
}

pub(crate) const CONFIDENCE_BANDS: [Confidence; 4] = [
    Confidence::Low,
    Confidence::Medium,
    Confidence::High,
    Confidence::Exact,
];

/// The serialized band name, which is how evidence JSON spells it.
pub(crate) fn confidence_band_name(band: Confidence) -> &'static str {
    match band {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
        Confidence::Exact => "exact",
    }
}

pub(crate) fn confidence_band_for_query_name(name: &str) -> Option<Confidence> {
    CONFIDENCE_BANDS
        .into_iter()
        .find(|band| name.eq_ignore_ascii_case(confidence_band_name(*band)))
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

pub(crate) fn evidence_source_types() -> Vec<String> {
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
        "edge_evidence_filters",
        "numeric_confidence_comparison",
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

// Every sentence here must describe `query.rs` as it behaves. The parser tests run every example
// against builder-shaped labels and reject every `unsupported` entry, but no test reads these
// sentences: change them together with the grammar.
fn query_syntax() -> Vec<String> {
    vec![
        "A query is MATCH <path> [WHERE <filter> [AND <filter>]...] RETURN <variable>[, <variable>]..., optionally followed by LIMIT <n> and OFFSET <n> in either order; keywords are case-insensitive.".into(),
        "A path is exactly one edge pattern between two nodes, such as (f:File)-[:DEFINES]->(s:Function) or (s:Function)<-[:DEFINES]-(f:File); a MATCH without an edge pattern is rejected.".into(),
        "A node is (variable:Type); the variable and the :Type are each optional, so (f), (:File) and () are nodes.".into(),
        "A one-hop edge is -[:TYPE]-> or <-[:TYPE]-, and it must name its type to run; -[e:TYPE]-> binds the edge to a variable that WHERE can filter on its evidence.".into(),
        format!("A multi-hop edge is -[:TYPE *min..max]-> with 1 <= min <= max, where max may not exceed the depth cap ({DEFAULT_MAX_DEPTH} unless raised, never above {HARD_MAX_DEPTH}); the :TYPE is optional, it binds no variable, the source node must name its type, and only forward edges are followed."),
        "Type names are case-insensitive and may be written as node_types and edge_types name them or in their underscored form: (t:DatabaseTable) or (t:database_table), [:DependsOn] or [:DEPENDS_ON].".into(),
        "A filter is variable.field = 'text', variable.field STARTS_WITH 'text', or variable.field =~ 'regex' on a variable bound in MATCH, with a single- or double-quoted value; confidence also takes <, <=, > and >=, and takes = or any of those with an unquoted number such as 0.85.".into(),
        "A File node's label is its repository-relative path (src/config.rs). A symbol node's label is that path without its extension, with / replaced by ::, followed by ::name (src::config::parse_config); this holds for every language, Java and Go included, with no package prefix and no segment dropped (src/main/java/com/acme/OrderService.java gives src::main::java::com::acme::OrderService::handle), except in a file where tree-sitter finds no symbols and a regex fallback names them. A label filter compares against that whole label, except that a one-hop label = filter also matches a bare symbol name (parse_config) through the index.".into(),
        filter_field_sentence(),
        confidence_sentence(),
        "=~ applies to label, file_path and qualified_name only, with a valid regex of at most 100 bytes.".into(),
        "RETURN lists node variables bound in MATCH, each at most once; an edge variable is for WHERE only, so read labels and properties from the returned node objects.".into(),
        format!("LIMIT is clamped to {HARD_ROW_LIMIT} rows, and a write-like or composition keyword (CREATE, MERGE, DELETE, DETACH, SET, REMOVE, DROP, CALL, LOAD, UNION, WITH, FOREACH) rejects the whole query."),
    ]
}

/// Built from the field tables the parser validates against.
fn filter_field_sentence() -> String {
    let symbol_types = SYMBOL_NODE_TYPES
        .iter()
        .map(node_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    let other_types = NODE_TYPES
        .iter()
        .filter(|node_type| {
            **node_type != GraphNodeType::File && !SYMBOL_NODE_TYPES.contains(*node_type)
        })
        .map(node_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Filter fields depend on what the variable binds. File nodes take {file}; file_path is the File node's label. {symbol_types} nodes take {symbol}; on a node built from an indexed symbol, qualified_name is its label and file_path is the label of the File node that DEFINES it, while a node of these types built from an import or an analysis fact carries neither. {other_types} nodes take {other}. An untyped node takes {symbol}. An edge variable takes {edge}, read from the edge's evidence. A filter on a field the variable does not take is a parse error listing the fields it takes; a filter on a field a matched node does not carry excludes that row, and a caveat counts the rows excluded that way.",
        file = FILE_FILTER_FIELDS.join(", "),
        symbol = SYMBOL_FILTER_FIELDS.join(", "),
        other = OTHER_NODE_FILTER_FIELDS.join(", "),
        edge = EDGE_FILTER_FIELDS.join(", "),
    )
}

fn confidence_sentence() -> String {
    let names = CONFIDENCE_BANDS
        .iter()
        .map(|band| confidence_band_name(*band))
        .collect::<Vec<_>>()
        .join(", ");
    let scores = CONFIDENCE_BANDS
        .iter()
        .map(|band| format!("{} {}", confidence_band_name(*band), band.score()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "confidence = 'high' compares the edge evidence's band ({names}); a number compares the band's score ({scores}), so e.confidence >= 0.85 keeps high and exact edges. source_type = takes a name from evidence_source_types, and source names the pass that recorded the evidence, such as open-kioku-graph or open-kioku-resolution."
    )
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
            "MATCH (caller:Function)-[:CALLS]->(callee:Function) WHERE callee.label = 'src::config::parse_config' RETURN caller",
            "Direct callers of parse_config in src/config.rs, named by its full qualified label.",
        ),
        (
            "MATCH (s:Function)<-[:DEFINES]-(f:File) WHERE s.label =~ '::handle_[^:]*$' RETURN s, f",
            "The DEFINES edge read in reverse: functions whose own name starts with handle_, and the files that define them. A regex filter is not anchored by the index, so this scans every DEFINES edge in memory and can reach the query timeout on a large index.",
        ),
        (
            "MATCH (a:Function)-[:CALLS *1..3]->(b:Function) WHERE a.label =~ '::run$' RETURN b LIMIT 20",
            "Functions reachable within one to three CALLS hops from functions named run. A multi-hop query walks forward from every Function node before its filters apply, so on a large index it can reach the query timeout; a one-hop query with label = is anchored by the index and much cheaper.",
        ),
        (
            "MATCH (f:File)-[:IMPORTS]->(g:File) WHERE g.file_path = 'src/config.rs' RETURN f",
            "Files whose imports resolve to src/config.rs.",
        ),
        (
            "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE b.file_path = 'src/config.rs' RETURN a, b",
            "Calls into functions defined in src/config.rs. On a symbol node file_path is the path of the File node that defines it, not the node's label.",
        ),
        (
            "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.confidence >= 0.85 AND c.source_type = 'tree_sitter' RETURN a, b",
            "Calls recorded from tree-sitter evidence at high or exact confidence. The edge variable c exists for WHERE only; source, source_type and confidence are read from the edge's evidence.",
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
            "edge_variable_in_return",
            "MATCH (a:Function)-[c:CALLS]->(b:Function) RETURN a, c",
            "RETURN node variables; an edge variable is bound only to filter the edge's evidence in WHERE, such as WHERE c.confidence >= 0.85.",
        ),
        unsupported(
            "edge_variable_on_hop_range",
            "MATCH (a:Function)-[c:CALLS *1..2]->(b:Function) RETURN b",
            "A hop range binds no single edge; bind and filter one hop at a time, such as (a:Function)-[c:CALLS]->(b:Function).",
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
            "Use =, STARTS_WITH or =~, and <, <=, > or >= on confidence; =~ 'parse' matches a substring of label, file_path or qualified_name.",
        ),
        unsupported(
            "unsupported_filter_field",
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.protocol = 'http' RETURN f",
            format!("Filter on a field the variable takes (File nodes take {}; the syntax lists the fields for every node type), and read other properties from the returned nodes.", FILE_FILTER_FIELDS.join(", ")),
        ),
        unsupported(
            "field_the_node_type_does_not_carry",
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.qualified_name = 'src::config::parse_config' RETURN s",
            "A File node has no qualified_name; filter the symbol instead: WHERE s.qualified_name = 'src::config::parse_config'.",
        ),
        unsupported(
            "evidence_field_on_a_node",
            "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE b.confidence >= 0.85 RETURN a",
            "Nodes carry no source, source_type or confidence; bind the edge and filter its evidence: MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.confidence >= 0.85 RETURN a.",
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

/// Every string listed in an `enum` array of a JSON schema. A documented variant appears under
/// `oneOf`, so the walk collects every `enum` array rather than only the top-level one.
#[cfg(test)]
pub(crate) fn enum_values(
    schema: &serde_json::Value,
    values: &mut std::collections::BTreeSet<String>,
) {
    match schema {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("enum", serde_json::Value::Array(items)) => values.extend(
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string)),
                    ),
                    _ => enum_values(child, values),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                enum_values(item, values);
            }
        }
        _ => {}
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

    // A variant left out of NODE_TYPES or EDGE_TYPES compiles, and the parser would then reject
    // it; the enums' JSON schemas are the independent source of every serialized variant. A
    // documented variant appears under `oneOf`, so the walk collects every `enum` array.
    #[test]
    fn type_lists_hold_every_variant() {
        let mut node_variants = std::collections::BTreeSet::new();
        enum_values(
            &serde_json::to_value(schemars::schema_for!(GraphNodeType)).unwrap(),
            &mut node_variants,
        );
        let listed_nodes = NODE_TYPES
            .iter()
            .map(node_type_query_spelling)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            listed_nodes.len(),
            NODE_TYPES.len(),
            "NODE_TYPES repeats a type"
        );
        assert_eq!(
            listed_nodes, node_variants,
            "NODE_TYPES must hold every GraphNodeType variant"
        );

        let mut edge_variants = std::collections::BTreeSet::new();
        enum_values(
            &serde_json::to_value(schemars::schema_for!(GraphEdgeType)).unwrap(),
            &mut edge_variants,
        );
        let listed_edges = EDGE_TYPES
            .iter()
            .map(edge_type_query_spelling)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            listed_edges.len(),
            EDGE_TYPES.len(),
            "EDGE_TYPES repeats a type"
        );
        assert_eq!(
            listed_edges, edge_variants,
            "EDGE_TYPES must hold every GraphEdgeType variant"
        );
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

    #[test]
    fn the_filter_field_sentence_names_every_node_type_and_every_field_list() {
        let sentence = filter_field_sentence();
        assert!(current_schema(None).syntax.contains(&sentence));
        for node_type in &NODE_TYPES {
            assert!(
                sentence.contains(node_type_name(node_type)),
                "{} is missing from: {sentence}",
                node_type_name(node_type)
            );
        }
        for fields in [
            FILE_FILTER_FIELDS,
            SYMBOL_FILTER_FIELDS,
            OTHER_NODE_FILTER_FIELDS,
            EDGE_FILTER_FIELDS,
        ] {
            assert!(sentence.contains(&fields.join(", ")), "{fields:?}");
        }
    }

    #[test]
    fn confidence_bands_are_every_serialized_band() {
        let mut variants = std::collections::BTreeSet::new();
        enum_values(
            &serde_json::to_value(schemars::schema_for!(Confidence)).unwrap(),
            &mut variants,
        );
        let listed = CONFIDENCE_BANDS
            .iter()
            .map(|band| confidence_band_name(*band).to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(listed, variants);
        for band in CONFIDENCE_BANDS {
            assert_eq!(
                serde_json::to_value(band).unwrap(),
                confidence_band_name(band)
            );
            assert_eq!(
                confidence_band_for_query_name(&confidence_band_name(band).to_ascii_uppercase()),
                Some(band)
            );
        }
    }

    // The parser validates `source_type =` by deserializing the value and lists these names when
    // it fails, so the list must be exactly the serialized variants.
    #[test]
    fn evidence_source_types_are_every_serialized_source_type() {
        let mut variants = std::collections::BTreeSet::new();
        enum_values(
            &serde_json::to_value(schemars::schema_for!(open_kioku_core::EvidenceSourceType))
                .unwrap(),
            &mut variants,
        );
        assert_eq!(
            evidence_source_types()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            variants
        );
    }
}
