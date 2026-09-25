use crate::schema::{
    confidence_band_for_query_name, confidence_band_name, edge_type_for_query_name, edge_type_name,
    evidence_source_types, node_filter_fields, node_type_for_query_name, node_type_name,
    CONFIDENCE_BANDS, EDGE_FILTER_FIELDS, EDGE_TYPES, NODE_TYPES,
};
use open_kioku_core::{
    Confidence, EvidenceSourceType, FileId, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType,
};
use open_kioku_errors::OkError;
use std::borrow::Cow;

pub type QueryResult<T> = std::result::Result<T, GraphQueryError>;
use open_kioku_storage::GraphStore;
use serde::Serialize;
use serde_json::Value;

/// The maximum depth for multi-hop graph queries.
pub const HARD_MAX_DEPTH: usize = 5;
/// The default depth for multi-hop graph queries.
pub const DEFAULT_MAX_DEPTH: usize = 3;

/// The hard row limit.
pub const HARD_ROW_LIMIT: usize = 500;
/// The default row limit.
pub const DEFAULT_ROW_LIMIT: usize = 50;
const EDGE_SCAN_BATCH_SIZE: usize = 1_000;
const NODE_SCAN_BATCH_SIZE: usize = 1_000;

#[derive(Debug, Clone, Serialize)]
pub struct GraphQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub returned: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
    pub warnings: Vec<String>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphQueryErrorResponse {
    pub error: GraphQueryErrorBody,
}

#[derive(Debug)]
pub enum GraphQueryError {
    ParseError(String),
    QueryRejected(String),
    UnknownNodeType(String),
    UnknownEdgeType(String),
    UnsupportedFilter(String),
    Timeout,
    DepthLimitExceeded(usize),
    LimitExceeded(usize),
    UnboundVariable(String),
    Storage(OkError),
    Serde(serde_json::Error),
}

impl std::fmt::Display for GraphQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseError(msg) => write!(f, "Parse error: {}", msg),
            Self::QueryRejected(msg) => write!(f, "Query rejected: {}", msg),
            Self::UnknownNodeType(msg) => write!(f, "Unknown node type: {}", msg),
            Self::UnknownEdgeType(msg) => write!(f, "Unknown edge type: {}", msg),
            Self::UnsupportedFilter(msg) => write!(f, "Unsupported filter field: {}", msg),
            Self::Timeout => write!(f, "Query execution timed out"),
            Self::DepthLimitExceeded(d) => write!(f, "Max depth exceeded: {}", d),
            Self::LimitExceeded(d) => write!(f, "Row limit exceeded: {}", d),
            Self::UnboundVariable(v) => write!(f, "Unbound variable: {}", v),
            Self::Storage(e) => write!(f, "Storage error: {}", e),
            Self::Serde(e) => write!(f, "Serde error: {}", e),
        }
    }
}

impl std::error::Error for GraphQueryError {}

impl From<OkError> for GraphQueryError {
    fn from(err: OkError) -> Self {
        Self::Storage(err)
    }
}

impl From<serde_json::Error> for GraphQueryError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serde(err)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphQueryErrorBody {
    pub kind: String,
    pub message: String,
    pub span: Option<QuerySpan>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuerySpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct GraphQueryOptions {
    pub limit: usize,
    pub offset: usize,
    pub max_depth: usize,
    pub deadline_ms: u64,
}

impl Default for GraphQueryOptions {
    fn default() -> Self {
        Self {
            limit: DEFAULT_ROW_LIMIT,
            offset: 0,
            max_depth: DEFAULT_MAX_DEPTH,
            deadline_ms: 500,
        }
    }
}

pub fn parse_graph_query(input: &str) -> QueryResult<GraphQueryAst> {
    let tokens = tokenize_with_columns(input)?;
    let mut parser = Parser::new(tokens);
    let ast = parser.parse()?;
    validate_ast(&ast)?;
    Ok(ast)
}

/// What a MATCH variable binds, which decides the WHERE fields it takes.
#[derive(Debug, Clone, Copy)]
enum Binding<'a> {
    Node(Option<&'a GraphNodeType>),
    Edge,
}

fn validate_ast(ast: &GraphQueryAst) -> QueryResult<()> {
    let mut bindings: HashMap<&str, Binding<'_>> = HashMap::new();
    let (source, target) = match &ast.match_clause.path {
        PathExpr::OneHop { source, target, .. } | PathExpr::MultiHop { source, target, .. } => {
            (source, target)
        }
    };
    for node in [source, target] {
        if let Some(variable) = &node.variable {
            // A pattern binding one variable to both ends stores the target in the row, so the
            // target's type is what a filter on it is checked against.
            bindings.insert(variable.as_str(), Binding::Node(node.node_type.as_ref()));
        }
    }

    match &ast.match_clause.path {
        PathExpr::OneHop { edge, .. } => {
            if let Some(variable) = &edge.variable {
                if bindings.insert(variable.as_str(), Binding::Edge).is_some() {
                    return Err(GraphQueryError::QueryRejected(format!(
                        "variable {variable} is bound to both a node and an edge"
                    )));
                }
            }
        }
        PathExpr::MultiHop { edge_range, .. } => {
            if edge_range.min_hops < 1 {
                return Err(GraphQueryError::QueryRejected(
                    "min_hops must be >= 1".into(),
                ));
            }
            if edge_range.max_hops < edge_range.min_hops {
                return Err(GraphQueryError::QueryRejected(
                    "max_hops must be >= min_hops".into(),
                ));
            }
            if edge_range.max_hops > HARD_MAX_DEPTH {
                return Err(GraphQueryError::QueryRejected(format!(
                    "max_hops cannot exceed hard limit of {}",
                    HARD_MAX_DEPTH
                )));
            }
        }
    }

    if let Some(where_clause) = &ast.where_clause {
        for filter in &where_clause.filters {
            let Some(binding) = bindings.get(filter.variable.as_str()) else {
                return Err(GraphQueryError::UnboundVariable(filter.variable.clone()));
            };
            validate_filter(filter, *binding)?;
        }
    }

    let mut returned_vars = std::collections::HashSet::new();
    for v in &ast.return_clause.variables {
        match bindings.get(v.as_str()) {
            None => return Err(GraphQueryError::UnboundVariable(v.clone())),
            Some(Binding::Edge) => {
                return Err(GraphQueryError::QueryRejected(format!(
                    "returning edge variables is not supported: {v}; RETURN node variables and filter the edge in WHERE"
                )))
            }
            Some(Binding::Node(_)) => {}
        }
        if !returned_vars.insert(v.clone()) {
            return Err(GraphQueryError::QueryRejected(format!(
                "duplicate variable in RETURN: {}",
                v
            )));
        }
    }

    Ok(())
}

/// Field and operator rules that depend on what the variable binds. Each rejection names what the
/// variable takes, so a filter that could never match is an error instead of an empty result that
/// reads as "no such relationship".
fn validate_filter(filter: &FilterExpr, binding: Binding<'_>) -> QueryResult<()> {
    let field = filter.field.as_str();
    let accepted = match binding {
        Binding::Node(node_type) => node_filter_fields(node_type),
        Binding::Edge => EDGE_FILTER_FIELDS,
    };
    if !accepted.contains(&field) {
        return Err(unknown_filter_field(filter, binding, accepted));
    }

    let operator = filter.operator.as_str();
    if field == "confidence" {
        return match (&filter.operator, &filter.value) {
            (FilterOperator::StartsWith | FilterOperator::RegexMatch, _) => {
                Err(GraphQueryError::ParseError(format!(
                    "confidence takes =, <, <=, > or >=, not {operator}; compare it with a band such as 'high' or a number such as 0.85"
                )))
            }
            (FilterOperator::Equals, FilterValue::Text(band)) => {
                if confidence_band_for_query_name(band).is_some() {
                    Ok(())
                } else {
                    Err(GraphQueryError::ParseError(format!(
                        "Unknown confidence: {band}; confidence is {}, or an unquoted number such as 0.85",
                        CONFIDENCE_BANDS
                            .iter()
                            .map(|band| confidence_band_name(*band))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )))
                }
            }
            (_, FilterValue::Text(_)) => Err(GraphQueryError::ParseError(format!(
                "`{operator}` compares numbers; write the value unquoted, such as {}.confidence {operator} 0.85",
                filter.variable
            ))),
            (_, FilterValue::Number(_)) => Ok(()),
        };
    }
    if filter.operator.is_comparison() {
        return Err(GraphQueryError::ParseError(format!(
            "`{operator}` compares numbers and applies to confidence only; {field} takes =, STARTS_WITH or =~"
        )));
    }
    let FilterValue::Text(value) = &filter.value else {
        return Err(GraphQueryError::ParseError(format!(
            "{field} takes a quoted string; unquoted numbers apply to confidence only"
        )));
    };
    if filter.operator == FilterOperator::RegexMatch
        && !["label", "qualified_name", "file_path"].contains(&field)
    {
        return Err(GraphQueryError::ParseError(
            "Regex filter only allowed on label, qualified_name, and file_path".into(),
        ));
    }
    if field == "evidence_source_type"
        && filter.operator == FilterOperator::Equals
        && serde_json::from_value::<EvidenceSourceType>(Value::String(value.clone())).is_err()
    {
        return Err(GraphQueryError::ParseError(format!(
            "Unknown evidence_source_type: {value}; evidence source types are {}",
            evidence_source_types().join(", ")
        )));
    }
    // `evidence_source` is the pass that recorded the evidence, not the node an edge came from. A
    // node label or id here parses, matches nothing, and reads as "nothing links these".
    if field == "evidence_source"
        && (value.starts_with("file:") || value.starts_with("symbol:") || value.contains("::"))
    {
        return Err(GraphQueryError::ParseError(format!(
            "{value} names a node, but evidence_source is the pass that recorded the edge's evidence, such as open-kioku-graph or open-kioku-resolution; to filter an endpoint, filter a node variable's label or id"
        )));
    }
    Ok(())
}

fn unknown_filter_field(
    filter: &FilterExpr,
    binding: Binding<'_>,
    accepted: &[&str],
) -> GraphQueryError {
    let field = filter.field.as_str();
    let holder = match binding {
        Binding::Node(Some(node_type)) => format!("{} nodes", node_type_name(node_type)),
        Binding::Node(None) => "nodes".to_string(),
        Binding::Edge => "edges".to_string(),
    };
    let hint = match binding {
        // `source` and `source_type` read as the edge's source node and that node's type. Neither is
        // a field on any binding; say what each reading is actually written as.
        _ if field == "source" => {
            "; an edge's source node is the node variable on its left: filter its label or id. The \
             pass that recorded an edge's evidence is evidence_source on a bound edge"
                .to_string()
        }
        _ if field == "source_type" => {
            "; an edge's source node type is written in the pattern, such as (a:Function). The kind \
             of evidence behind an edge is evidence_source_type on a bound edge"
                .to_string()
        }
        Binding::Node(_) if EDGE_FILTER_FIELDS.contains(&field) => format!(
            "; {field} is read from edge evidence: bind the edge as -[e:TYPE]-> and filter e.{field}"
        ),
        // An edge has an id; it simply is not one of the fields a filter may name.
        Binding::Edge if field == "id" => {
            "; an edge has an id, but only its evidence fields can be filtered".to_string()
        }
        Binding::Edge if node_filter_fields(None).contains(&field) => {
            format!("; {field} is a node field: filter it on a node variable")
        }
        _ => String::new(),
    };
    GraphQueryError::ParseError(format!(
        "Unknown filter field: {}.{field}; {holder} filter on {}{hint}",
        filter.variable,
        accepted.join(", ")
    ))
}

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub fn execute_graph_query(
    store: &dyn GraphStore,
    query: &GraphQueryAst,
    options: GraphQueryOptions,
) -> QueryResult<GraphQueryResult> {
    let limit = query.limit.unwrap_or(options.limit).min(HARD_ROW_LIMIT);
    let offset = query.offset.unwrap_or(options.offset);
    let target_rows = offset.saturating_add(limit).saturating_add(1);

    let start_time = Instant::now();
    let deadline = Duration::from_millis(options.deadline_ms);

    let mut warnings = Vec::new();
    if query.limit.unwrap_or(0) > HARD_ROW_LIMIT {
        warnings.push(format!("LIMIT clamped to {}", HARD_ROW_LIMIT));
    }

    let mut rows: Vec<HashMap<String, Bound>> = Vec::new();
    let MatchClause { path } = &query.match_clause;
    let mut used_indexed_anchor = false;
    let mut filters = FilterEvaluator::new(store, query.where_clause.as_ref());

    let check_node = |node: &open_kioku_core::GraphNode, expr: &NodeExpr| -> bool {
        if let Some(t) = &expr.node_type {
            if &node.node_type != t {
                return false;
            }
        }
        true
    };

    match path {
        PathExpr::OneHop {
            source,
            edge,
            target,
        } => {
            let edge_type = if let Some(t) = &edge.edge_type {
                t.clone()
            } else {
                return Err(GraphQueryError::ParseError(
                    "OneHop path requires an edge type for initial candidate narrowing".into(),
                ));
            };

            let anchor_filter = query.where_clause.as_ref().and_then(|where_clause| {
                where_clause.filters.iter().find_map(|filter| {
                    let FilterValue::Text(value) = &filter.value else {
                        return None;
                    };
                    if filter.operator != FilterOperator::Equals
                        || !matches!(filter.field.as_str(), "id" | "label")
                    {
                        return None;
                    }
                    if source.variable.as_deref() == Some(filter.variable.as_str()) {
                        Some((true, filter, value.as_str()))
                    } else if target.variable.as_deref() == Some(filter.variable.as_str()) {
                        Some((false, filter, value.as_str()))
                    } else {
                        None
                    }
                })
            });

            let mut anchored_edges = Vec::new();
            if let Some((anchor_is_source, filter, anchor_value)) = anchor_filter {
                let anchor_expr = if anchor_is_source { source } else { target };
                let anchor_nodes = if filter.field == "id" {
                    match store.node_by_id(anchor_value) {
                        Ok(node) => Some(node.into_iter().collect::<Vec<_>>()),
                        Err(OkError::Unsupported(_)) => None,
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    let mut nodes = Vec::new();
                    let mut node_offset = 0;
                    loop {
                        match store.nodes_by_label(
                            anchor_value,
                            anchor_expr.node_type.clone(),
                            NODE_SCAN_BATCH_SIZE,
                            node_offset,
                        ) {
                            Ok(batch) => {
                                let batch_len = batch.len();
                                nodes.extend(batch);
                                if batch_len < NODE_SCAN_BATCH_SIZE {
                                    break;
                                }
                                node_offset = node_offset.saturating_add(NODE_SCAN_BATCH_SIZE);
                            }
                            Err(OkError::Unsupported(_)) => {
                                nodes.clear();
                                break;
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    if nodes.is_empty() && node_offset == 0 {
                        match store.nodes_by_label(
                            anchor_value,
                            anchor_expr.node_type.clone(),
                            1,
                            0,
                        ) {
                            Err(OkError::Unsupported(_)) => None,
                            Err(error) => return Err(error.into()),
                            Ok(_) => Some(nodes),
                        }
                    } else {
                        Some(nodes)
                    }
                };

                if let Some(anchor_nodes) = anchor_nodes {
                    used_indexed_anchor = true;
                    'anchors: for anchor in anchor_nodes {
                        if start_time.elapsed() > deadline {
                            return Err(GraphQueryError::Timeout);
                        }
                        if !check_node(&anchor, anchor_expr) {
                            continue;
                        }
                        let outgoing = match (anchor_is_source, &edge.direction) {
                            (true, Direction::Forward) | (false, Direction::Reverse) => true,
                            (true, Direction::Reverse) | (false, Direction::Forward) => false,
                        };
                        let mut edge_offset = 0;
                        loop {
                            match store.edges_by_type_for_node(
                                edge_type.clone(),
                                &anchor.id.0,
                                outgoing,
                                EDGE_SCAN_BATCH_SIZE,
                                edge_offset,
                            ) {
                                Ok(batch) => {
                                    let batch_len = batch.len();
                                    anchored_edges.extend(batch);
                                    if batch_len < EDGE_SCAN_BATCH_SIZE {
                                        break;
                                    }
                                    edge_offset = edge_offset.saturating_add(EDGE_SCAN_BATCH_SIZE);
                                }
                                Err(OkError::Unsupported(_)) => {
                                    used_indexed_anchor = false;
                                    anchored_edges.clear();
                                    break 'anchors;
                                }
                                Err(error) => return Err(error.into()),
                            }
                        }
                    }
                    anchored_edges.sort_by(|left, right| left.id.0.cmp(&right.id.0));
                    anchored_edges.dedup_by(|left, right| left.id == right.id);
                }
            }

            let mut curr_offset = 0;
            let mut anchored_offset: usize = 0;
            let skip_anchor_filter = if used_indexed_anchor {
                anchor_filter.map(|(_, filter, _)| filter)
            } else {
                None
            };

            'paging: while rows.len() < target_rows {
                if start_time.elapsed() > deadline {
                    return Err(GraphQueryError::Timeout);
                }

                let batch = if used_indexed_anchor {
                    let end = anchored_offset
                        .saturating_add(EDGE_SCAN_BATCH_SIZE)
                        .min(anchored_edges.len());
                    let batch = anchored_edges[anchored_offset..end].to_vec();
                    anchored_offset = end;
                    batch
                } else {
                    let batch = store.edges_by_type(
                        edge_type.clone(),
                        EDGE_SCAN_BATCH_SIZE,
                        curr_offset,
                    )?;
                    curr_offset = curr_offset.saturating_add(EDGE_SCAN_BATCH_SIZE);
                    batch
                };
                if batch.is_empty() {
                    break;
                }

                // With a file_path filter, the batch's rows are staged so their symbols' defining
                // files come from one store read instead of one per symbol.
                let mut staged = Vec::new();
                for e in batch {
                    if start_time.elapsed() > deadline {
                        return Err(GraphQueryError::Timeout);
                    }
                    if rows.len() >= target_rows {
                        break 'paging;
                    }

                    let source_id = &e.from.0;
                    let target_id = &e.to.0;

                    let (actual_source_id, actual_target_id) = match edge.direction {
                        Direction::Forward => (source_id, target_id),
                        Direction::Reverse => (target_id, source_id),
                    };

                    let actual_source = match store.node_by_id(actual_source_id)? {
                        Some(n) => n,
                        None => continue,
                    };
                    let actual_target = match store.node_by_id(actual_target_id)? {
                        Some(n) => n,
                        None => continue,
                    };

                    if !check_node(&actual_source, source) {
                        continue;
                    }
                    if !check_node(&actual_target, target) {
                        continue;
                    }

                    let mut row = HashMap::new();
                    if let Some(v) = &source.variable {
                        row.insert(v.clone(), Bound::Node(actual_source));
                    }
                    if let Some(v) = &target.variable {
                        row.insert(v.clone(), Bound::Node(actual_target));
                    }
                    if let Some(v) = &edge.variable {
                        row.insert(v.clone(), Bound::Edge(e));
                    }

                    if filters.stages_rows() {
                        staged.push(row);
                    } else if filters.matches(&row, skip_anchor_filter)? {
                        rows.push(row);
                    }
                }
                filters.resolve_symbol_files(&staged)?;
                for row in staged {
                    if rows.len() >= target_rows {
                        break 'paging;
                    }
                    if filters.matches(&row, skip_anchor_filter)? {
                        rows.push(row);
                    }
                }
            }
        }
        PathExpr::MultiHop {
            source,
            edge_range,
            target,
        } => {
            if edge_range.max_hops > options.max_depth {
                return Err(GraphQueryError::DepthLimitExceeded(edge_range.max_hops));
            }
            if edge_range.max_hops > HARD_MAX_DEPTH {
                return Err(GraphQueryError::ParseError(format!(
                    "Max hops {} exceeds hard max depth {}",
                    edge_range.max_hops, HARD_MAX_DEPTH
                )));
            }
            if edge_range.direction == Direction::Reverse {
                return Err(GraphQueryError::ParseError(
                    "Reverse multi-hop not supported".into(),
                ));
            }

            let source_type = if let Some(t) = &source.node_type {
                t.clone()
            } else {
                return Err(GraphQueryError::ParseError(
                    "MultiHop requires a source node type for initial narrowing".into(),
                ));
            };

            let mut start_offset = 0;
            'starts: loop {
                if start_time.elapsed() > deadline {
                    return Err(GraphQueryError::Timeout);
                }

                let start_nodes =
                    store.nodes_by_type(source_type.clone(), NODE_SCAN_BATCH_SIZE, start_offset)?;
                if start_nodes.is_empty() {
                    break;
                }
                start_offset = start_offset.saturating_add(NODE_SCAN_BATCH_SIZE);

                for start_node in start_nodes {
                    if rows.len() >= target_rows {
                        break 'starts;
                    }
                    if start_time.elapsed() > deadline {
                        return Err(GraphQueryError::Timeout);
                    }

                    // Rows from one start node are staged the same way as a one-hop edge batch.
                    let mut staged = Vec::new();
                    let mut queue = std::collections::VecDeque::new();
                    let mut visited = std::collections::HashSet::new();

                    queue.push_back((start_node.clone(), 0));

                    while let Some((curr_node, depth)) = queue.pop_front() {
                        if rows.len() >= target_rows {
                            break;
                        }
                        // Staged rows reach `rows` only when a batch is flushed, so this loop needs
                        // its own deadline check: the checks outside it cannot fire until a start
                        // node's whole closure is walked, and the caller would see its own timeout
                        // instead of the `timeout` this query reports.
                        if start_time.elapsed() > deadline {
                            return Err(GraphQueryError::Timeout);
                        }
                        if !visited.insert((curr_node.id.0.clone(), depth)) {
                            continue;
                        }

                        if depth >= edge_range.min_hops
                            && depth <= edge_range.max_hops
                            && check_node(&curr_node, target)
                        {
                            let mut row = HashMap::new();
                            if let Some(v) = &source.variable {
                                row.insert(v.clone(), Bound::Node(start_node.clone()));
                            }
                            if let Some(v) = &target.variable {
                                row.insert(v.clone(), Bound::Node(curr_node.clone()));
                            }
                            if filters.stages_rows() {
                                staged.push(row);
                            } else if filters.matches(&row, None)? {
                                rows.push(row);
                            }
                        }

                        if depth < edge_range.max_hops {
                            let (_, edges) =
                                store.neighbors(&curr_node.id.0, EDGE_SCAN_BATCH_SIZE)?;
                            for edge in edges {
                                // Follow only forward edges for multi-hop
                                if edge.from.0 != curr_node.id.0 {
                                    continue;
                                }

                                if let Some(expected_type) = &edge_range.edge_type {
                                    if &edge.edge_type != expected_type {
                                        continue;
                                    }
                                }

                                if !visited.contains(&(edge.to.0.clone(), depth + 1)) {
                                    if let Some(next) = store.node_by_id(&edge.to.0)? {
                                        queue.push_back((next, depth + 1));
                                    }
                                }
                            }
                        }

                        // Flush a full batch rather than staging the whole closure: this bounds the
                        // staged rows and lets `rows` reach the page, which is what ends the walk.
                        if staged.len() >= EDGE_SCAN_BATCH_SIZE {
                            filters.resolve_symbol_files(&staged)?;
                            for row in staged.drain(..) {
                                if rows.len() >= target_rows {
                                    break;
                                }
                                if filters.matches(&row, None)? {
                                    rows.push(row);
                                }
                            }
                        }
                    }
                    filters.resolve_symbol_files(&staged)?;
                    for row in staged {
                        if rows.len() >= target_rows {
                            break 'starts;
                        }
                        if filters.matches(&row, None)? {
                            rows.push(row);
                        }
                    }
                }
            }
        }
    }

    let has_more = rows.len() > offset.saturating_add(limit);
    let paginated_rows = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    let mut final_rows = Vec::new();
    let columns = query.return_clause.variables.clone();

    for row in paginated_rows {
        let mut out_row = Vec::new();
        for col in &columns {
            out_row.push(match row.get(col) {
                Some(bound) => bound.to_value()?,
                None => serde_json::Value::Null,
            });
        }
        final_rows.push(serde_json::Value::Array(out_row));
    }

    let mut caveats = vec![if used_indexed_anchor {
        "Equality filter anchored by indexed node and edge lookup.".into()
    } else {
        "Filters applied in-memory after indexed edge lookup.".into()
    }];
    caveats.extend(filters.absent_field_caveats());

    let returned = final_rows.len();
    Ok(GraphQueryResult {
        columns,
        rows: final_rows,
        returned,
        limit,
        offset,
        has_more,
        warnings,
        caveats,
    })
}

/// A MATCH variable's value in one row. Rows hold the typed node or edge so WHERE reads the field it
/// names; they are serialized only for RETURN.
enum Bound {
    Node(GraphNode),
    Edge(GraphEdge),
}

impl Bound {
    fn to_value(&self) -> serde_json::Result<Value> {
        match self {
            Self::Node(node) => serde_json::to_value(node),
            Self::Edge(edge) => serde_json::to_value(edge),
        }
    }
}

/// Evaluates WHERE against rows. Parsing has already checked every field against what its variable
/// binds; what remains is per node, because a Module or Endpoint node built from an import or an
/// analysis fact carries no qualified_name or file_path while one built from a symbol does. Rows
/// excluded for that reason are counted so the result can say so.
struct FilterEvaluator<'a> {
    store: &'a dyn GraphStore,
    filters: &'a [FilterExpr],
    /// The defining File node's path per symbol node id; `None` unless exactly one File node
    /// DEFINES the symbol.
    symbol_files: HashMap<String, Option<String>>,
    /// File id and label per File node id, shared by every symbol the file defines.
    file_nodes: HashMap<String, Option<(Option<FileId>, String)>>,
    /// Candidate rows excluded because a filtered node field was absent, per field.
    absent: std::collections::BTreeMap<String, usize>,
    /// Whether rows are staged so a file_path filter reads defining files in one store read per
    /// batch; cleared when the store has no batched read.
    stage_file_paths: bool,
}

impl<'a> FilterEvaluator<'a> {
    fn new(store: &'a dyn GraphStore, where_clause: Option<&'a WhereClause>) -> Self {
        let filters = where_clause
            .map(|clause| clause.filters.as_slice())
            .unwrap_or_default();
        Self {
            store,
            filters,
            symbol_files: HashMap::new(),
            file_nodes: HashMap::new(),
            absent: std::collections::BTreeMap::new(),
            stage_file_paths: filters.iter().any(|filter| filter.field == "file_path"),
        }
    }

    fn stages_rows(&self) -> bool {
        self.stage_file_paths
    }

    /// Reads the DEFINES edges of every uncached symbol node that a file_path filter reads in
    /// `rows` with one store call, so evaluating the rows finds each defining file in the cache.
    fn resolve_symbol_files(&mut self, rows: &[HashMap<String, Bound>]) -> QueryResult<()> {
        if !self.stage_file_paths || rows.is_empty() {
            return Ok(());
        }
        let filters = self.filters;
        let mut symbols: HashMap<&str, &GraphNode> = HashMap::new();
        for filter in filters.iter().filter(|filter| filter.field == "file_path") {
            for row in rows {
                if let Some(Bound::Node(node)) = row.get(&filter.variable) {
                    if node.node_type != GraphNodeType::File
                        && node.symbol_id.is_some()
                        && !self.symbol_files.contains_key(&node.id.0)
                    {
                        symbols.entry(node.id.0.as_str()).or_insert(node);
                    }
                }
            }
        }
        if symbols.is_empty() {
            return Ok(());
        }
        let mut ids = symbols.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        let edges = match self
            .store
            .edges_by_type_for_nodes(GraphEdgeType::Defines, &ids, false)
        {
            Ok(edges) => edges,
            // Without a batched read, each symbol is looked up when its row is evaluated.
            Err(OkError::Unsupported(_)) => {
                self.stage_file_paths = false;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let mut defining: HashMap<&str, Vec<String>> = HashMap::new();
        for edge in &edges {
            defining
                .entry(edge.to.0.as_str())
                .or_default()
                .push(edge.from.0.clone());
        }
        for id in ids {
            let files = defining.remove(id).unwrap_or_default();
            self.record_symbol_file(symbols[id], &files)?;
        }
        Ok(())
    }

    fn matches(
        &mut self,
        row: &HashMap<String, Bound>,
        skip_filter: Option<&FilterExpr>,
    ) -> QueryResult<bool> {
        let filters = self.filters;
        for filter in filters {
            if skip_filter.is_some_and(|skip| std::ptr::eq(skip, filter)) {
                continue;
            }
            let matched = match row.get(&filter.variable) {
                None => false,
                Some(Bound::Edge(edge)) => edge_matches(edge, filter),
                Some(Bound::Node(node)) => match self.node_field(node, &filter.field)? {
                    Some(value) => text_matches(filter, &value),
                    None => {
                        *self.absent.entry(filter.field.clone()).or_default() += 1;
                        false
                    }
                },
            };
            if !matched {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn node_field<'n>(
        &mut self,
        node: &'n GraphNode,
        field: &str,
    ) -> QueryResult<Option<Cow<'n, str>>> {
        let from_symbol = node.symbol_id.is_some();
        Ok(match field {
            "label" => Some(Cow::Borrowed(node.label.as_str())),
            "id" => Some(Cow::Borrowed(node.id.0.as_str())),
            "file_path" if node.node_type == GraphNodeType::File => {
                Some(Cow::Borrowed(node.label.as_str()))
            }
            "file_path" if from_symbol => self.symbol_file_path(node)?.map(Cow::Owned),
            // The graph builder copies the symbol's qualified name into the label.
            "qualified_name" if from_symbol => Some(Cow::Borrowed(node.label.as_str())),
            _ => None,
        })
    }

    /// A symbol node's label is derived from its path but drops the extension and rewrites `/`, so
    /// the path is read from the File node that DEFINES the symbol. `GraphNode.file_id` names the
    /// file record rather than carrying its path, so the File node is the only place the path
    /// lives: a symbol node reachable without its DEFINES edge would have no `file_path`.
    fn symbol_file_path(&mut self, symbol: &GraphNode) -> QueryResult<Option<String>> {
        if let Some(path) = self.symbol_files.get(&symbol.id.0) {
            return Ok(path.clone());
        }
        let mut defining = Vec::new();
        let mut offset = 0;
        loop {
            let batch = self.store.edges_by_type_for_node(
                GraphEdgeType::Defines,
                &symbol.id.0,
                false,
                EDGE_SCAN_BATCH_SIZE,
                offset,
            )?;
            let batch_len = batch.len();
            defining.extend(batch.into_iter().map(|edge| edge.from.0));
            if batch_len < EDGE_SCAN_BATCH_SIZE {
                break;
            }
            offset = offset.saturating_add(EDGE_SCAN_BATCH_SIZE);
        }
        self.record_symbol_file(symbol, &defining)
    }

    /// Caches and returns the path of the one File node among `defining`, the DEFINES sources of
    /// `symbol`, or `None` when there is not exactly one.
    fn record_symbol_file(
        &mut self,
        symbol: &GraphNode,
        defining: &[String],
    ) -> QueryResult<Option<String>> {
        let mut paths = std::collections::BTreeSet::new();
        for file_node_id in defining {
            if let Some((file_id, label)) = self.file_node(file_node_id)? {
                // A File node recorded for another file id is not where this symbol lives.
                if file_id.is_none() || symbol.file_id.is_none() || file_id == symbol.file_id {
                    paths.insert(label);
                }
            }
        }
        // With two defining files any one path would be a guess, so the field is absent instead.
        let path = if paths.len() == 1 {
            paths.pop_first()
        } else {
            None
        };
        self.symbol_files.insert(symbol.id.0.clone(), path.clone());
        Ok(path)
    }

    fn file_node(&mut self, id: &str) -> QueryResult<Option<(Option<FileId>, String)>> {
        if let Some(file) = self.file_nodes.get(id) {
            return Ok(file.clone());
        }
        let file = self
            .store
            .node_by_id(id)?
            .filter(|node| node.node_type == GraphNodeType::File)
            .map(|node| (node.file_id, node.label));
        self.file_nodes.insert(id.to_string(), file.clone());
        Ok(file)
    }

    fn absent_field_caveats(&self) -> Vec<String> {
        self.absent
            .iter()
            .map(|(field, rows)| {
                let carriers = match field.as_str() {
                    "file_path" => {
                        "file_path is carried by File nodes and by symbol nodes that exactly one File node DEFINES"
                    }
                    "qualified_name" => {
                        "qualified_name is carried by nodes built from an indexed symbol"
                    }
                    _ => "the field is not recorded on those nodes",
                };
                // A lower bound, not a total: `absent` counts only rows the scan reached, and the
                // scan stops once the requested page is filled. Stating it as an exact count would
                // read as the whole size of the evidence gap.
                format!(
                    "At least {rows} scanned row(s) were excluded because the filtered node carries no {field}; the scan stops once the requested page is filled, so rows beyond it were not counted. {carriers}."
                )
            })
            .collect()
    }
}

fn text_matches(filter: &FilterExpr, value: &str) -> bool {
    let FilterValue::Text(expected) = &filter.value else {
        return false;
    };
    match filter.operator {
        FilterOperator::Equals => value == expected.as_str(),
        FilterOperator::StartsWith => value.starts_with(expected.as_str()),
        FilterOperator::RegexMatch => {
            regex::Regex::new(expected).is_ok_and(|pattern| pattern.is_match(value))
        }
        // Parsing restricts comparisons to confidence.
        FilterOperator::LessThan
        | FilterOperator::LessOrEqual
        | FilterOperator::GreaterThan
        | FilterOperator::GreaterOrEqual => false,
    }
}

/// Graph nodes carry no evidence, so evidence_source, evidence_source_type and confidence come
/// from a bound edge.
fn edge_matches(edge: &GraphEdge, filter: &FilterExpr) -> bool {
    let evidence = &edge.evidence;
    match filter.field.as_str() {
        "evidence_source" => text_matches(filter, evidence.source.as_str()),
        "evidence_source_type" => serde_json::to_value(&evidence.source_type)
            .ok()
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|name| text_matches(filter, name)),
        "confidence" => confidence_matches(evidence.confidence, filter),
        _ => false,
    }
}

/// A band name compares the band. A number compares `Confidence::score` in `f32`, the type the
/// literal was parsed into, so `0.85` is exactly the high band's score rather than a hair below
/// its widened `f64` value.
fn confidence_matches(band: Confidence, filter: &FilterExpr) -> bool {
    // The band's score, deliberately, not `Evidence.confidence_score`: storage round-trips that
    // field but no producer records one today, and reading it for the few edges that had it while
    // the rest fell back to the band would make a single threshold mean two different things. A
    // producer that starts recording per-edge scores has to revisit this comparison.
    let score = band.score();
    match (&filter.operator, &filter.value) {
        (FilterOperator::Equals, FilterValue::Text(name)) => {
            confidence_band_for_query_name(name) == Some(band)
        }
        (FilterOperator::Equals, FilterValue::Number(number)) => score == *number,
        (FilterOperator::LessThan, FilterValue::Number(number)) => score < *number,
        (FilterOperator::LessOrEqual, FilterValue::Number(number)) => score <= *number,
        (FilterOperator::GreaterThan, FilterValue::Number(number)) => score > *number,
        (FilterOperator::GreaterOrEqual, FilterValue::Number(number)) => score >= *number,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphQueryAst {
    pub match_clause: MatchClause,
    pub where_clause: Option<WhereClause>,
    pub return_clause: ReturnClause,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    pub path: PathExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathExpr {
    OneHop {
        source: NodeExpr,
        edge: EdgeExpr,
        target: NodeExpr,
    },
    MultiHop {
        source: NodeExpr,
        edge_range: EdgeRangeExpr,
        target: NodeExpr,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeExpr {
    pub variable: Option<String>,
    pub node_type: Option<GraphNodeType>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeExpr {
    pub direction: Direction,
    pub edge_type: Option<GraphEdgeType>,
    pub variable: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRangeExpr {
    pub direction: Direction,
    pub edge_type: Option<GraphEdgeType>,
    pub min_hops: usize,
    pub max_hops: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Forward, // -[...]-\>
    Reverse, // \<-[...]-
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhereClause {
    pub filters: Vec<FilterExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterExpr {
    pub variable: String,
    pub field: String,
    pub operator: FilterOperator,
    pub value: FilterValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterOperator {
    Equals,
    StartsWith,
    RegexMatch,
    LessThan,
    LessOrEqual,
    GreaterThan,
    GreaterOrEqual,
}

impl FilterOperator {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Equals => "=",
            Self::StartsWith => "STARTS_WITH",
            Self::RegexMatch => "=~",
            Self::LessThan => "<",
            Self::LessOrEqual => "<=",
            Self::GreaterThan => ">",
            Self::GreaterOrEqual => ">=",
        }
    }

    fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::LessThan | Self::LessOrEqual | Self::GreaterThan | Self::GreaterOrEqual
        )
    }
}

/// A filter's right-hand side: a quoted string, or an unquoted number, which only confidence takes.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    Text(String),
    /// `f32`, the type `Confidence::score` returns, so a written `0.85` equals the high band's
    /// score exactly.
    Number(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub variables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Match,
    Where,
    Return,
    Limit,
    Offset,
    And,
    StartsWith,
    Identifier(String),
    StringLiteral(String),
    IntLiteral(usize),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dash,
    ArrowRight,
    ArrowLeft,
    Colon,
    Dot,
    Comma,
    Equals,
    RegexMatch,
    Asterisk,
    DotDot,
    /// A number with a fractional part, such as `0.85`; whole numbers are `IntLiteral`.
    NumberLiteral(String),
    LessThan,
    LessOrEqual,
    GreaterThan,
    GreaterOrEqual,
}

pub fn tokenize(input: &str) -> QueryResult<Vec<Token>> {
    Ok(tokenize_with_columns(input)?
        .into_iter()
        .map(|(token, _)| token)
        .collect())
}

/// Each token with the 1-based column it starts at, so a parse error can point at it.
fn tokenize_with_columns(input: &str) -> QueryResult<Vec<(Token, usize)>> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().enumerate().peekable();

    while let Some(&(index, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }

        let token = match c {
            '(' => {
                chars.next();
                Token::LParen
            }
            ')' => {
                chars.next();
                Token::RParen
            }
            '[' => {
                chars.next();
                Token::LBracket
            }
            ']' => {
                chars.next();
                Token::RBracket
            }
            ':' => {
                chars.next();
                Token::Colon
            }
            ',' => {
                chars.next();
                Token::Comma
            }
            '*' => {
                chars.next();
                Token::Asterisk
            }
            '=' => {
                chars.next();
                if let Some(&(_, '~')) = chars.peek() {
                    chars.next();
                    Token::RegexMatch
                } else {
                    Token::Equals
                }
            }
            '.' => {
                chars.next();
                if let Some(&(_, '.')) = chars.peek() {
                    chars.next();
                    Token::DotDot
                } else {
                    Token::Dot
                }
            }
            '-' => {
                chars.next();
                if let Some(&(_, '>')) = chars.peek() {
                    chars.next();
                    Token::ArrowRight
                } else {
                    Token::Dash
                }
            }
            '<' => {
                chars.next();
                match chars.peek() {
                    Some(&(_, '-')) => {
                        chars.next();
                        Token::ArrowLeft
                    }
                    Some(&(_, '=')) => {
                        chars.next();
                        Token::LessOrEqual
                    }
                    _ => Token::LessThan,
                }
            }
            '>' => {
                chars.next();
                if let Some(&(_, '=')) = chars.peek() {
                    chars.next();
                    Token::GreaterOrEqual
                } else {
                    Token::GreaterThan
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut string_lit = String::new();
                let mut closed = false;
                while let Some(&(_, next_c)) = chars.peek() {
                    if next_c == quote {
                        chars.next();
                        closed = true;
                        break;
                    }
                    string_lit.push(next_c);
                    chars.next();
                }
                if !closed {
                    return Err(GraphQueryError::ParseError(
                        "Unclosed string literal".into(),
                    ));
                }
                Token::StringLiteral(string_lit)
            }
            _ if c.is_ascii_digit() => {
                let mut num_str = String::new();
                take_digits(&mut chars, &mut num_str);
                // A fraction needs a digit after its dot, so the `..` of a hop range stays a range.
                let mut ahead = chars.clone();
                let fraction = matches!(ahead.next(), Some((_, '.')))
                    && matches!(ahead.next(), Some((_, digit)) if digit.is_ascii_digit());
                if fraction {
                    chars.next();
                    num_str.push('.');
                    take_digits(&mut chars, &mut num_str);
                    Token::NumberLiteral(num_str)
                } else {
                    let val: usize = num_str.parse().map_err(|_| {
                        GraphQueryError::ParseError(format!("Invalid integer: {}", num_str))
                    })?;
                    Token::IntLiteral(val)
                }
            }
            _ if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&(_, next_c)) = chars.peek() {
                    if next_c.is_ascii_alphanumeric() || next_c == '_' {
                        ident.push(next_c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match ident.to_uppercase().as_str() {
                    "MATCH" => Token::Match,
                    "WHERE" => Token::Where,
                    "RETURN" => Token::Return,
                    "LIMIT" => Token::Limit,
                    "OFFSET" => Token::Offset,
                    "AND" => Token::And,
                    "STARTS_WITH" => Token::StartsWith,
                    "CREATE" | "MERGE" | "DELETE" | "DETACH" | "SET" | "REMOVE" | "DROP"
                    | "CALL" | "LOAD" | "UNION" | "WITH" | "FOREACH" => {
                        return Err(GraphQueryError::ParseError(format!(
                            "Write-like or unsupported keyword rejected: {}",
                            ident
                        )));
                    }
                    _ => Token::Identifier(ident),
                }
            }
            _ => {
                return Err(GraphQueryError::ParseError(format!(
                    "Unexpected character: {}",
                    c
                )));
            }
        };
        tokens.push((token, index + 1));
    }

    Ok(tokens)
}

fn take_digits(
    chars: &mut std::iter::Peekable<std::iter::Enumerate<std::str::Chars<'_>>>,
    into: &mut String,
) {
    while let Some(&(_, next_c)) = chars.peek() {
        if !next_c.is_ascii_digit() {
            break;
        }
        into.push(next_c);
        chars.next();
    }
}

/// The token as a query would spell it, for error messages.
fn token_text(token: &Token) -> String {
    match token {
        Token::Match => "MATCH".into(),
        Token::Where => "WHERE".into(),
        Token::Return => "RETURN".into(),
        Token::Limit => "LIMIT".into(),
        Token::Offset => "OFFSET".into(),
        Token::And => "AND".into(),
        Token::StartsWith => "STARTS_WITH".into(),
        Token::Identifier(name) => name.clone(),
        Token::StringLiteral(value) => format!("'{value}'"),
        Token::IntLiteral(value) => value.to_string(),
        Token::LParen => "(".into(),
        Token::RParen => ")".into(),
        Token::LBracket => "[".into(),
        Token::RBracket => "]".into(),
        Token::Dash => "-".into(),
        Token::ArrowRight => "->".into(),
        Token::ArrowLeft => "<-".into(),
        Token::Colon => ":".into(),
        Token::Dot => ".".into(),
        Token::Comma => ",".into(),
        Token::Equals => "=".into(),
        Token::RegexMatch => "=~".into(),
        Token::Asterisk => "*".into(),
        Token::DotDot => "..".into(),
        Token::NumberLiteral(value) => value.clone(),
        Token::LessThan => "<".into(),
        Token::LessOrEqual => "<=".into(),
        Token::GreaterThan => ">".into(),
        Token::GreaterOrEqual => ">=".into(),
    }
}

fn missing_edge_pattern() -> GraphQueryError {
    GraphQueryError::ParseError(
        "MATCH needs an edge pattern such as (a:File)-[:DEFINES]->(b:Function); isolated node patterns are not supported".into(),
    )
}

fn return_expression_error() -> GraphQueryError {
    GraphQueryError::ParseError(
        "RETURN accepts only variables bound in MATCH; functions, DISTINCT and AS aliases are not supported".into(),
    )
}

fn unknown_node_type(name: &str) -> GraphQueryError {
    let accepted = NODE_TYPES
        .iter()
        .map(node_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    GraphQueryError::ParseError(format!(
        "Unknown node type: {name}; node types are {accepted}"
    ))
}

fn unknown_edge_type(name: &str) -> GraphQueryError {
    let accepted = EDGE_TYPES
        .iter()
        .map(edge_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    GraphQueryError::ParseError(format!(
        "Unknown edge type: {name}; edge types are {accepted}"
    ))
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<(Token, usize)>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|(token, _)| token)
    }

    /// Column of the token `peek` returns; 0 once the input is exhausted.
    fn column(&self) -> usize {
        self.tokens.get(self.pos).map_or(0, |(_, column)| *column)
    }

    fn consume(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos).map(|(token, _)| token);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, expected: Token) -> QueryResult<()> {
        match self.consume() {
            Some(t) if t == &expected => Ok(()),
            Some(t) => Err(GraphQueryError::ParseError(format!(
                "Expected {:?}, got {:?}",
                expected, t
            ))),
            None => Err(GraphQueryError::ParseError(format!(
                "Expected {:?}, got EOF",
                expected
            ))),
        }
    }

    fn parse(&mut self) -> QueryResult<GraphQueryAst> {
        let match_clause = self.parse_match()?;
        let where_clause = if let Some(Token::Where) = self.peek() {
            Some(self.parse_where()?)
        } else {
            None
        };
        let return_clause = self.parse_return()?;
        let mut limit = None;
        let mut offset = None;

        while let Some(t) = self.peek() {
            match t {
                Token::Limit => limit = Some(self.parse_limit()?),
                Token::Offset => offset = Some(self.parse_offset()?),
                _ => {
                    return Err(GraphQueryError::ParseError(format!(
                        "Unexpected token `{}` at column {}; only LIMIT <n> and OFFSET <n> may follow RETURN",
                        token_text(t),
                        self.column()
                    )))
                }
            }
        }

        Ok(GraphQueryAst {
            match_clause,
            where_clause,
            return_clause,
            limit,
            offset,
        })
    }

    fn parse_match(&mut self) -> QueryResult<MatchClause> {
        self.expect(Token::Match)?;
        let path = self.parse_path()?;
        Ok(MatchClause { path })
    }

    fn parse_path(&mut self) -> QueryResult<PathExpr> {
        let source = self.parse_node()?;

        let is_reverse = match self.peek() {
            Some(Token::Dash) => false,
            Some(Token::ArrowLeft) => true,
            _ => return Err(missing_edge_pattern()),
        };
        self.consume();
        self.expect(Token::LBracket)?;

        let mut edge_variable = None;
        if let Some(Token::Identifier(name)) = self.peek() {
            let name = name.clone();
            self.consume();
            if !matches!(self.peek(), Some(Token::Colon))
                && edge_type_for_query_name(&name).is_some()
            {
                return Err(GraphQueryError::ParseError(format!(
                    "Write the edge type after a colon: -[:{name}]->; -[{name}]-> would bind a variable named {name}"
                )));
            }
            edge_variable = Some(name);
        }

        let mut edge_type = None;
        if let Some(Token::Colon) = self.peek() {
            self.consume();
            if let Some(Token::Identifier(s)) = self.peek() {
                let s = s.clone();
                self.consume();
                edge_type =
                    Some(edge_type_for_query_name(&s).ok_or_else(|| unknown_edge_type(&s))?);
            }
        }

        if let Some(Token::Asterisk) = self.peek() {
            if edge_variable.is_some() {
                return Err(GraphQueryError::ParseError(
                    "A hop range binds no single edge, so -[e:TYPE *min..max]-> cannot name a variable; bind and filter one hop with -[e:TYPE]->".into(),
                ));
            }
            self.consume();
            let min_hops = match self.consume() {
                Some(Token::IntLiteral(n)) => *n,
                _ => {
                    return Err(GraphQueryError::ParseError(
                        "Expected integer min hops; give a hop range such as *1..3".into(),
                    ))
                }
            };
            self.expect(Token::DotDot)?;
            let max_hops = match self.consume() {
                Some(Token::IntLiteral(n)) => *n,
                _ => {
                    return Err(GraphQueryError::ParseError(
                        "Expected integer max hops; give a hop range such as *1..3".into(),
                    ))
                }
            };
            self.expect(Token::RBracket)?;

            let direction = if is_reverse {
                self.expect(Token::Dash)?;
                Direction::Reverse
            } else {
                self.expect(Token::ArrowRight)?;
                Direction::Forward
            };

            if is_reverse {
                return Err(GraphQueryError::ParseError(
                    "Reverse multi-hop not supported; write (a)<-[:TYPE *min..max]-(b) forward as (b)-[:TYPE *min..max]->(a)".into(),
                ));
            }

            let target = self.parse_node()?;
            return Ok(PathExpr::MultiHop {
                source,
                edge_range: EdgeRangeExpr {
                    direction,
                    edge_type,
                    min_hops,
                    max_hops,
                },
                target,
            });
        }

        self.expect(Token::RBracket)?;
        let direction = if is_reverse {
            self.expect(Token::Dash)?;
            Direction::Reverse
        } else {
            self.expect(Token::ArrowRight)?;
            Direction::Forward
        };

        let target = self.parse_node()?;
        Ok(PathExpr::OneHop {
            source,
            edge: EdgeExpr {
                direction,
                edge_type,
                variable: edge_variable,
            },
            target,
        })
    }

    fn parse_node(&mut self) -> QueryResult<NodeExpr> {
        self.expect(Token::LParen)?;
        let mut variable = None;
        let mut node_type = None;

        if let Some(Token::Identifier(s)) = self.peek() {
            variable = Some(s.clone());
            self.consume();
        }

        if let Some(Token::Colon) = self.peek() {
            self.consume();
            if let Some(Token::Identifier(s)) = self.consume() {
                node_type = Some(node_type_for_query_name(s).ok_or_else(|| unknown_node_type(s))?);
            } else {
                return Err(GraphQueryError::ParseError("Expected node type".into()));
            }
        }

        self.expect(Token::RParen)?;
        Ok(NodeExpr {
            variable,
            node_type,
        })
    }

    fn parse_where(&mut self) -> QueryResult<WhereClause> {
        self.expect(Token::Where)?;
        let mut filters = Vec::new();
        filters.push(self.parse_filter()?);
        while let Some(Token::And) = self.peek() {
            self.consume();
            filters.push(self.parse_filter()?);
        }
        Ok(WhereClause { filters })
    }

    fn parse_filter(&mut self) -> QueryResult<FilterExpr> {
        let variable = match self.consume() {
            Some(Token::Identifier(s)) => s.clone(),
            _ => return Err(GraphQueryError::ParseError("Expected identifier".into())),
        };
        self.expect(Token::Dot)?;
        let field = match self.consume() {
            Some(Token::Identifier(s)) => s.clone(),
            _ => return Err(GraphQueryError::ParseError("Expected field name".into())),
        };

        // Which fields a variable takes depends on what MATCH binds it to, so fields are checked in
        // `validate_filter` once the whole query is parsed.
        let operator = match self.consume() {
            Some(Token::Equals) => FilterOperator::Equals,
            Some(Token::StartsWith) => FilterOperator::StartsWith,
            Some(Token::RegexMatch) => FilterOperator::RegexMatch,
            Some(Token::LessThan) => FilterOperator::LessThan,
            Some(Token::LessOrEqual) => FilterOperator::LessOrEqual,
            Some(Token::GreaterThan) => FilterOperator::GreaterThan,
            Some(Token::GreaterOrEqual) => FilterOperator::GreaterOrEqual,
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected =, STARTS_WITH, =~, <, <=, > or >=".into(),
                ))
            }
        };

        let value = match self.consume() {
            Some(Token::StringLiteral(s)) => FilterValue::Text(s.clone()),
            Some(Token::IntLiteral(n)) => FilterValue::Number(*n as f32),
            Some(Token::NumberLiteral(text)) => match text.parse::<f32>() {
                Ok(number) if number.is_finite() => FilterValue::Number(number),
                _ => {
                    return Err(GraphQueryError::ParseError(format!(
                        "Invalid number: {text}"
                    )))
                }
            },
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected a quoted string, or an unquoted number for confidence".into(),
                ))
            }
        };

        if let (FilterOperator::RegexMatch, FilterValue::Text(pattern)) = (&operator, &value) {
            if pattern.len() > 100 {
                return Err(GraphQueryError::ParseError("Regex pattern too long".into()));
            }
            if regex::Regex::new(pattern).is_err() {
                return Err(GraphQueryError::ParseError("Invalid regex pattern".into()));
            }
        }

        Ok(FilterExpr {
            variable,
            field,
            operator,
            value,
        })
    }

    fn parse_return(&mut self) -> QueryResult<ReturnClause> {
        self.expect(Token::Return)?;
        let mut variables = Vec::new();
        let variable = match self.consume() {
            Some(Token::Identifier(s)) => s.clone(),
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected identifier in RETURN".into(),
                ))
            }
        };
        self.reject_return_expression(&variable)?;
        variables.push(variable);
        while let Some(Token::Comma) = self.peek() {
            self.consume();
            let variable = match self.consume() {
                Some(Token::Identifier(s)) => s.clone(),
                _ => {
                    return Err(GraphQueryError::ParseError(
                        "Expected identifier after comma".into(),
                    ))
                }
            };
            self.reject_return_expression(&variable)?;
            variables.push(variable);
        }
        Ok(ReturnClause { variables })
    }

    /// RETURN takes bare variables. Property access, functions, DISTINCT and AS aliases are named
    /// here, so the error says what RETURN accepts instead of reporting trailing input.
    fn reject_return_expression(&self, variable: &str) -> QueryResult<()> {
        match self.peek() {
            Some(Token::Dot) => Err(GraphQueryError::ParseError(
                "RETURN accepts variables only; filter properties in WHERE".into(),
            )),
            Some(Token::LParen) => Err(return_expression_error()),
            Some(Token::Identifier(next))
                if variable.eq_ignore_ascii_case("DISTINCT") || next.eq_ignore_ascii_case("AS") =>
            {
                Err(return_expression_error())
            }
            _ => Ok(()),
        }
    }

    fn parse_limit(&mut self) -> QueryResult<usize> {
        self.expect(Token::Limit)?;
        match self.consume() {
            Some(Token::IntLiteral(n)) => Ok(*n),
            _ => Err(GraphQueryError::ParseError("Expected integer limit".into())),
        }
    }

    fn parse_offset(&mut self) -> QueryResult<usize> {
        self.expect(Token::Offset)?;
        match self.consume() {
            Some(Token::IntLiteral(n)) => Ok(*n),
            _ => Err(GraphQueryError::ParseError(
                "Expected integer offset".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{GraphEdgeType, GraphNodeType};

    #[test]
    fn test_parse_one_hop_directed_match() {
        let q = "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s";
        let ast = parse_graph_query(q).unwrap();

        let PathExpr::OneHop {
            source,
            edge,
            target,
        } = ast.match_clause.path
        else {
            panic!()
        };
        assert_eq!(source.variable.unwrap(), "f");
        assert_eq!(source.node_type.unwrap(), GraphNodeType::File);
        assert_eq!(edge.direction, Direction::Forward);
        assert_eq!(edge.edge_type.unwrap(), GraphEdgeType::Defines);
        assert_eq!(target.variable.unwrap(), "s");
        assert_eq!(target.node_type.unwrap(), GraphNodeType::Function);

        assert_eq!(
            ast.return_clause.variables,
            vec!["f".to_string(), "s".to_string()]
        );
    }

    #[test]
    fn test_parse_reverse_edge_match() {
        let q = "MATCH (f:File)<-[:DEFINES]-(s:Function) RETURN f";
        let ast = parse_graph_query(q).unwrap();

        let PathExpr::OneHop {
            source: _,
            edge,
            target: _,
        } = ast.match_clause.path
        else {
            panic!()
        };
        assert_eq!(edge.direction, Direction::Reverse);
        assert_eq!(edge.edge_type.unwrap(), GraphEdgeType::Defines);
    }

    #[test]
    fn test_parse_bounded_multi_hop_match() {
        let q = "MATCH (f:File)-[:DEPENDS_ON *1..3]->(t:Test) RETURN f, t";
        let ast = parse_graph_query(q).unwrap();

        let PathExpr::MultiHop {
            source: _,
            edge_range,
            target: _,
        } = ast.match_clause.path
        else {
            panic!()
        };
        assert_eq!(edge_range.min_hops, 1);
        assert_eq!(edge_range.max_hops, 3);
        assert_eq!(edge_range.direction, Direction::Forward);
    }

    #[test]
    fn test_reject_unbounded_variable_length_path() {
        let res = parse_graph_query("MATCH (f)-[:DEPENDS_ON *]->(t) RETURN f");
        assert!(res.is_err()); // Parse error because `*` without range isn't matching
    }

    #[test]
    fn test_reject_write_keywords() {
        let res = parse_graph_query("CREATE (n) RETURN n");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("Write-like or unsupported keyword rejected"));

        let res2 = parse_graph_query("MATCH (n) DELETE n");
        assert!(res2.is_err());
    }

    #[test]
    fn test_reject_raw_sql_injection() {
        let res =
            parse_graph_query("MATCH (n)-[]->(m) WHERE n.id = '1'; DROP TABLE graph_nodes; --'");
        assert!(res.is_err());
    }

    #[test]
    fn test_offset_supported() {
        let ast = parse_graph_query("MATCH (n)-[]->(m) RETURN n OFFSET 10").unwrap();
        assert_eq!(ast.offset, Some(10));
    }

    #[test]
    fn test_regex_filter_only_allowed_on_label_qualified_name_file_path() {
        let ast = parse_graph_query("MATCH (n)-[]->(m) WHERE n.label =~ 'a.*' RETURN n").unwrap();
        assert_eq!(
            ast.where_clause.unwrap().filters[0].operator,
            FilterOperator::RegexMatch
        );

        let res = parse_graph_query("MATCH (n)-[]->(m) WHERE n.id =~ 'a.*' RETURN n");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("only allowed on label, qualified_name, and file_path"));
    }

    #[test]
    fn test_property_prefix_filter() {
        let ast =
            parse_graph_query("MATCH (n)-[]->(m) WHERE n.file_path STARTS_WITH 'src/' RETURN n")
                .unwrap();
        assert_eq!(
            ast.where_clause.unwrap().filters[0].operator,
            FilterOperator::StartsWith
        );
    }

    #[test]
    fn test_unknown_node_type_rejected() {
        let res = parse_graph_query("MATCH (n:SuperNode) RETURN n");
        assert!(res.is_err());
    }

    #[test]
    fn test_unknown_edge_type_rejected() {
        let res = parse_graph_query("MATCH (n)-[:SUPER_EDGE]->(m) RETURN n");
        assert!(res.is_err());
    }

    struct MockGraphStore {
        nodes: std::collections::HashMap<String, open_kioku_core::GraphNode>,
        edges: Vec<open_kioku_core::GraphEdge>,
    }

    impl open_kioku_storage::GraphStore for MockGraphStore {
        fn replace_graph(
            &self,
            _nodes: &[open_kioku_core::GraphNode],
            _edges: &[open_kioku_core::GraphEdge],
        ) -> open_kioku_errors::Result<()> {
            Ok(())
        }
        fn node_by_id(
            &self,
            id: &str,
        ) -> open_kioku_errors::Result<Option<open_kioku_core::GraphNode>> {
            Ok(self.nodes.get(id).cloned())
        }
        fn neighbors(
            &self,
            node: &str,
            _limit: usize,
        ) -> open_kioku_errors::Result<(
            Vec<open_kioku_core::GraphNode>,
            Vec<open_kioku_core::GraphEdge>,
        )> {
            let edges: Vec<_> = self
                .edges
                .iter()
                .filter(|e| e.from.0 == node || e.to.0 == node)
                .cloned()
                .collect();
            let mut nodes = Vec::new();
            for e in &edges {
                if e.from.0 != node {
                    if let Some(n) = self.nodes.get(&e.from.0) {
                        nodes.push(n.clone());
                    }
                }
                if e.to.0 != node {
                    if let Some(n) = self.nodes.get(&e.to.0) {
                        nodes.push(n.clone());
                    }
                }
            }
            Ok((nodes, edges))
        }
        fn shortest_path(
            &self,
            _from: &str,
            _to: &str,
            _max_depth: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            Ok(vec![])
        }
        fn nodes_by_type(
            &self,
            node_type: GraphNodeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphNode>> {
            Ok(self
                .nodes
                .values()
                .filter(|n| n.node_type == node_type)
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn nodes_by_label(
            &self,
            label: &str,
            node_type: Option<GraphNodeType>,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphNode>> {
            Ok(self
                .nodes
                .values()
                .filter(|node| {
                    node.label == label
                        && node_type
                            .as_ref()
                            .is_none_or(|expected| &node.node_type == expected)
                })
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn edges_by_type(
            &self,
            edge_type: GraphEdgeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            Ok(self
                .edges
                .iter()
                .filter(|edge| edge.edge_type == edge_type)
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn edges_by_type_for_node(
            &self,
            edge_type: GraphEdgeType,
            node_id: &str,
            outgoing: bool,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            Ok(self
                .edges
                .iter()
                .filter(|edge| {
                    edge.edge_type == edge_type
                        && if outgoing {
                            edge.from.0 == node_id
                        } else {
                            edge.to.0 == node_id
                        }
                })
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn edges_by_type_for_nodes(
            &self,
            edge_type: GraphEdgeType,
            node_ids: &[&str],
            outgoing: bool,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            Ok(self
                .edges
                .iter()
                .filter(|edge| {
                    let endpoint = if outgoing { &edge.from.0 } else { &edge.to.0 };
                    edge.edge_type == edge_type && node_ids.contains(&endpoint.as_str())
                })
                .cloned()
                .collect())
        }
    }

    /// Delegates to a MockGraphStore, counting DEFINES reads, and can withhold the batched read.
    struct CountingStore {
        inner: MockGraphStore,
        batched: bool,
        batched_reads: std::sync::atomic::AtomicUsize,
        per_symbol_reads: std::sync::atomic::AtomicUsize,
    }

    impl open_kioku_storage::GraphStore for CountingStore {
        fn replace_graph(
            &self,
            nodes: &[open_kioku_core::GraphNode],
            edges: &[open_kioku_core::GraphEdge],
        ) -> open_kioku_errors::Result<()> {
            self.inner.replace_graph(nodes, edges)
        }
        fn node_by_id(
            &self,
            id: &str,
        ) -> open_kioku_errors::Result<Option<open_kioku_core::GraphNode>> {
            self.inner.node_by_id(id)
        }
        fn neighbors(
            &self,
            node: &str,
            limit: usize,
        ) -> open_kioku_errors::Result<(
            Vec<open_kioku_core::GraphNode>,
            Vec<open_kioku_core::GraphEdge>,
        )> {
            self.inner.neighbors(node, limit)
        }
        fn shortest_path(
            &self,
            from: &str,
            to: &str,
            max_depth: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            self.inner.shortest_path(from, to, max_depth)
        }
        fn nodes_by_type(
            &self,
            node_type: GraphNodeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphNode>> {
            self.inner.nodes_by_type(node_type, limit, offset)
        }
        fn edges_by_type(
            &self,
            edge_type: GraphEdgeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            self.inner.edges_by_type(edge_type, limit, offset)
        }
        fn edges_by_type_for_node(
            &self,
            edge_type: GraphEdgeType,
            node_id: &str,
            outgoing: bool,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            if edge_type == GraphEdgeType::Defines {
                self.per_symbol_reads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.inner
                .edges_by_type_for_node(edge_type, node_id, outgoing, limit, offset)
        }
        fn edges_by_type_for_nodes(
            &self,
            edge_type: GraphEdgeType,
            node_ids: &[&str],
            outgoing: bool,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            if !self.batched {
                return Err(OkError::Unsupported("no batched read".into()));
            }
            self.batched_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner
                .edges_by_type_for_nodes(edge_type, node_ids, outgoing)
        }
    }

    /// Delegates to a MockGraphStore, pausing on each neighbour read so a traversal outlasts a
    /// short deadline without depending on how fast the machine is.
    struct SlowNeighborsStore {
        inner: MockGraphStore,
    }

    impl open_kioku_storage::GraphStore for SlowNeighborsStore {
        fn replace_graph(
            &self,
            nodes: &[open_kioku_core::GraphNode],
            edges: &[open_kioku_core::GraphEdge],
        ) -> open_kioku_errors::Result<()> {
            self.inner.replace_graph(nodes, edges)
        }
        fn node_by_id(
            &self,
            id: &str,
        ) -> open_kioku_errors::Result<Option<open_kioku_core::GraphNode>> {
            self.inner.node_by_id(id)
        }
        fn neighbors(
            &self,
            node: &str,
            limit: usize,
        ) -> open_kioku_errors::Result<(
            Vec<open_kioku_core::GraphNode>,
            Vec<open_kioku_core::GraphEdge>,
        )> {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.inner.neighbors(node, limit)
        }
        fn shortest_path(
            &self,
            from: &str,
            to: &str,
            max_depth: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            self.inner.shortest_path(from, to, max_depth)
        }
        fn nodes_by_type(
            &self,
            node_type: GraphNodeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphNode>> {
            self.inner.nodes_by_type(node_type, limit, offset)
        }
        fn edges_by_type(
            &self,
            edge_type: GraphEdgeType,
            limit: usize,
            offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphEdge>> {
            self.inner.edges_by_type(edge_type, limit, offset)
        }
    }

    /// One File start node whose DEPENDS_ON chain is `depth` hops long and `width` wide per hop.
    fn chain_store(width: usize, depth: usize) -> MockGraphStore {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store
            .nodes
            .insert("file:root".into(), file_node("file:root", "crates/root.rs"));
        let mut frontier = vec!["file:root".to_string()];
        for level in 0..depth {
            let mut next = Vec::new();
            for parent in &frontier {
                for index in 0..width {
                    let id = format!("file:l{level}-{}-{index}", next.len());
                    store
                        .nodes
                        .insert(id.clone(), file_node(&id, &format!("crates/{id}.rs")));
                    store.edges.push(test_edge(
                        &format!("edge-{id}"),
                        parent,
                        &id,
                        GraphEdgeType::DependsOn,
                    ));
                    next.push(id);
                }
            }
            frontier = next;
        }
        store
    }

    // A file_path filter stages rows, so without the in-loop deadline check a traversal runs to the
    // end of a start node's closure before any deadline is consulted: the caller hits its own
    // timeout instead of this one. Every other deadline check sits outside the traversal, so they
    // would also report a timeout here — eventually. What distinguishes the in-loop check is when:
    // this closure needs ~150 paused neighbour reads to walk, and the query must give up during it,
    // not after it. Neutralising the in-loop check makes the elapsed assertion fail.
    #[test]
    fn a_staged_multi_hop_times_out_inside_the_traversal() {
        let store = SlowNeighborsStore {
            inner: single_start_chain_store(12, 3),
        };
        assert_eq!(
            store
                .inner
                .nodes
                .values()
                .filter(|node| node.node_type == GraphNodeType::File)
                .count(),
            1,
            "one start node, so the traversal is the only place left to spend the deadline"
        );
        let query = parse_graph_query(
            "MATCH (f:File)-[:DEPENDS_ON *1..3]->(g:Function) WHERE g.file_path STARTS_WITH 'crates/' RETURN g LIMIT 50",
        )
        .unwrap();
        let started = Instant::now();
        let error = execute_graph_query(
            &store,
            &query,
            GraphQueryOptions {
                deadline_ms: 20,
                ..GraphQueryOptions::default()
            },
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            matches!(error, GraphQueryError::Timeout),
            "expected a timeout, got {error}"
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "the traversal must be abandoned part-way, not completed first; took {elapsed:?}"
        );
    }

    /// One File node, the start of a DEPENDS_ON tree whose other nodes are Functions, so a MATCH
    /// from `(f:File)` has exactly one start node.
    fn single_start_chain_store(width: usize, depth: usize) -> MockGraphStore {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store
            .nodes
            .insert("file:root".into(), file_node("file:root", "crates/root.rs"));
        let mut frontier = vec!["file:root".to_string()];
        for level in 0..depth {
            let mut next = Vec::new();
            for parent in &frontier {
                for index in 0..width {
                    let id = format!("fn:l{level}-{}-{index}", next.len());
                    store.nodes.insert(
                        id.clone(),
                        test_node(&id, &format!("crates::{id}"), GraphNodeType::Function),
                    );
                    store.edges.push(test_edge(
                        &format!("edge-{id}"),
                        parent,
                        &id,
                        GraphEdgeType::DependsOn,
                    ));
                    next.push(id);
                }
            }
            frontier = next;
        }
        store
    }

    // The staged path must still return the page it was asked for.
    #[test]
    fn a_staged_multi_hop_returns_its_page() {
        let store = chain_store(4, 3);
        let result = run(
            &store,
            "MATCH (f:File)-[:DEPENDS_ON *1..3]->(g:File) WHERE g.file_path STARTS_WITH 'crates/' RETURN g LIMIT 10",
        );
        assert_eq!(result.returned, 10);
        assert!(result.has_more);
        assert!(result
            .rows
            .iter()
            .all(|row| row[0]["label"].as_str().unwrap().starts_with("crates/")));
    }

    #[test]
    fn file_path_on_symbol_nodes_reads_defining_files_once_per_batch() {
        let query =
            "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE a.file_path STARTS_WITH 'src/' \
                     AND b.file_path STARTS_WITH 'src/' RETURN a, b";
        for batched in [true, false] {
            let store = CountingStore {
                inner: example_store(),
                batched,
                batched_reads: std::sync::atomic::AtomicUsize::new(0),
                per_symbol_reads: std::sync::atomic::AtomicUsize::new(0),
            };
            let result = run(&store, query);
            assert_eq!(
                column_ids(&result, 0),
                ["fn:parse_config", "fn:run"],
                "batched: {batched}"
            );
            let reads = (
                store
                    .batched_reads
                    .load(std::sync::atomic::Ordering::SeqCst),
                store
                    .per_symbol_reads
                    .load(std::sync::atomic::Ordering::SeqCst),
            );
            // Both CALLS edges arrive in one scanned batch naming three distinct symbols.
            let expected = if batched { (1, 0) } else { (0, 3) };
            assert_eq!(reads, expected, "batched: {batched}");
        }
    }

    fn test_node(id: &str, label: &str, node_type: GraphNodeType) -> open_kioku_core::GraphNode {
        open_kioku_core::GraphNode {
            id: open_kioku_core::NodeId::new(id),
            node_type,
            label: label.into(),
            ..Default::default()
        }
    }

    fn test_edge(
        id: &str,
        from: &str,
        to: &str,
        edge_type: GraphEdgeType,
    ) -> open_kioku_core::GraphEdge {
        open_kioku_core::GraphEdge {
            id: open_kioku_core::EdgeId::new(id),
            from: open_kioku_core::NodeId::new(from),
            to: open_kioku_core::NodeId::new(to),
            edge_type,
            ..Default::default()
        }
    }

    #[test]
    fn test_execute_multi_hop_edge_direction_and_type() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store
            .nodes
            .insert("f1".into(), test_node("f1", "A", GraphNodeType::Function));
        store
            .nodes
            .insert("f2".into(), test_node("f2", "B", GraphNodeType::Function));
        store
            .edges
            .push(test_edge("e1", "f1", "f2", GraphEdgeType::Calls));
        store
            .edges
            .push(test_edge("e2", "f1", "f2", GraphEdgeType::Imports));

        let query =
            parse_graph_query("MATCH (a:Function)-[:CALLS *1..2]->(b:Function) RETURN a, b")
                .unwrap();
        let res = execute_graph_query(
            &store as &dyn open_kioku_storage::GraphStore,
            &query,
            GraphQueryOptions::default(),
        )
        .unwrap();
        assert_eq!(res.rows.len(), 1);
    }

    #[test]
    fn test_multihop_rejects_min_zero() {
        let res = parse_graph_query("MATCH (a:Function)-[:CALLS *0..2]->(b:Function) RETURN a, b");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("min_hops must be >= 1"));
    }

    #[test]
    fn test_multihop_rejects_min_greater_than_max() {
        let res = parse_graph_query("MATCH (a:Function)-[:CALLS *3..2]->(b:Function) RETURN a, b");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("max_hops must be >= min_hops"));
    }

    #[test]
    fn test_multihop_allows_same_node_at_different_depths_when_range_requires_it() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store
            .nodes
            .insert("f1".into(), test_node("f1", "A", GraphNodeType::Function));
        store
            .nodes
            .insert("f2".into(), test_node("f2", "B", GraphNodeType::Function));
        // f1 -> f2 (1 hop)
        store
            .edges
            .push(test_edge("e1", "f1", "f2", GraphEdgeType::Calls));
        // f2 -> f1 (2 hops total: f1 -> f2 -> f1)
        store
            .edges
            .push(test_edge("e2", "f2", "f1", GraphEdgeType::Calls));

        // 2..3 should allow reaching f1 again at depth 2
        let query =
            parse_graph_query("MATCH (a:Function)-[:CALLS *2..3]->(b:Function) RETURN a, b")
                .unwrap();
        let res = execute_graph_query(
            &store as &dyn open_kioku_storage::GraphStore,
            &query,
            GraphQueryOptions::default(),
        )
        .unwrap();
        // Since we go f1->f2 (depth 1, ignored) -> f1 (depth 2, matched) -> f2 (depth 3, matched)
        // AND f2->f1 (depth 1, ignored) -> f2 (depth 2, matched) -> f1 (depth 3, matched)
        // The total number of paths is 4.
        assert_eq!(res.rows.len(), 4);
    }

    #[test]
    fn test_one_hop_reports_has_more_by_fetching_one_extra_match() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store.nodes.insert(
            "root".into(),
            test_node("root", "root", GraphNodeType::File),
        );
        for idx in 0..3 {
            let id = format!("fn{idx}");
            store
                .nodes
                .insert(id.clone(), test_node(&id, &id, GraphNodeType::Function));
            store.edges.push(test_edge(
                &format!("edge{idx}"),
                "root",
                &id,
                GraphEdgeType::Defines,
            ));
        }

        let query =
            parse_graph_query("MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 2")
                .unwrap();
        let res = execute_graph_query(
            &store as &dyn open_kioku_storage::GraphStore,
            &query,
            GraphQueryOptions::default(),
        )
        .unwrap();
        assert_eq!(res.rows.len(), 2);
        assert!(res.has_more);
    }

    #[test]
    fn test_one_hop_equality_filter_uses_indexed_anchor() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store.nodes.insert(
            "root".into(),
            test_node("root", "root", GraphNodeType::File),
        );
        for idx in 0..3 {
            let id = format!("fn{idx}");
            store.nodes.insert(
                id.clone(),
                test_node(&id, &format!("function-{idx}"), GraphNodeType::Function),
            );
            store.edges.push(test_edge(
                &format!("edge{idx}"),
                "root",
                &id,
                GraphEdgeType::Defines,
            ));
        }

        let query = parse_graph_query(
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.label = 'function-1' RETURN f, s",
        )
        .unwrap();
        let result = execute_graph_query(&store, &query, GraphQueryOptions::default()).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][1]["id"], "fn1");
        assert_eq!(
            result.caveats,
            vec!["Equality filter anchored by indexed node and edge lookup."]
        );
    }

    #[test]
    fn test_multihop_reports_has_more_by_fetching_one_extra_match() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store.nodes.insert(
            "root".into(),
            test_node("root", "root", GraphNodeType::File),
        );
        for idx in 0..3 {
            let id = format!("fn{idx}");
            store
                .nodes
                .insert(id.clone(), test_node(&id, &id, GraphNodeType::Function));
            store.edges.push(test_edge(
                &format!("edge{idx}"),
                "root",
                &id,
                GraphEdgeType::Defines,
            ));
        }

        let query =
            parse_graph_query("MATCH (f:File)-[:DEFINES *1..1]->(s:Function) RETURN f, s LIMIT 2")
                .unwrap();
        let res = execute_graph_query(
            &store as &dyn open_kioku_storage::GraphStore,
            &query,
            GraphQueryOptions::default(),
        )
        .unwrap();
        assert_eq!(res.rows.len(), 2);
        assert!(res.has_more);
    }

    /// Labels are shaped as an index writes them. A File node carries its repository-relative path.
    /// A symbol node carries the qualified name tree-sitter gives it, which the graph builder copies
    /// into the label: the path without its extension, `/` as `::`, then `::name`, in every language.
    /// `symbol_label` mirrors `qualified_name` in open-kioku-tree-sitter; the Java node pins that no
    /// package prefix is applied, which `identity::qualified_name` would add.
    fn example_store() -> MockGraphStore {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        let symbol_label = |path: &str, name: &str| {
            let stem = std::path::Path::new(path)
                .with_extension("")
                .to_string_lossy()
                .replace(['/', '\\'], "::");
            format!("{stem}::{name}")
        };
        let java_path = "src/main/java/com/acme/OrderService.java";
        for (id, path) in [
            ("file:app", "src/app.rs"),
            ("file:config", "src/config.rs"),
            ("file:order_service", java_path),
        ] {
            store.nodes.insert(id.into(), file_node(id, path));
        }
        // Each symbol node carries its file's id and a DEFINES edge from that File node, as the
        // graph builder writes them.
        for (id, file, name, node_type) in [
            ("fn:run", "file:app", "run", GraphNodeType::Function),
            (
                "fn:handle_request",
                "file:app",
                "handle_request",
                GraphNodeType::Function,
            ),
            (
                "fn:parse_config",
                "file:config",
                "parse_config",
                GraphNodeType::Function,
            ),
            (
                "method:handle_order",
                "file:order_service",
                "handle_order",
                GraphNodeType::Method,
            ),
        ] {
            let label = symbol_label(&store.nodes[file].label, name);
            store
                .nodes
                .insert(id.into(), symbol_node(id, &label, node_type, file));
            store.edges.push(evidence_edge(
                &format!("defines-{id}"),
                file,
                id,
                GraphEdgeType::Defines,
                "open-kioku-graph",
                EvidenceSourceType::TreeSitter,
                Confidence::Exact,
            ));
        }
        assert_eq!(
            store.nodes["fn:parse_config"].label,
            "src::config::parse_config"
        );
        assert_eq!(
            store.nodes["method:handle_order"].label,
            "src::main::java::com::acme::OrderService::handle_order"
        );

        for (id, from, to, edge_type, source, source_type, confidence) in [
            (
                "calls-parse",
                "fn:run",
                "fn:parse_config",
                GraphEdgeType::Calls,
                "open-kioku-resolution",
                EvidenceSourceType::TreeSitter,
                Confidence::High,
            ),
            (
                "calls-handle",
                "fn:parse_config",
                "fn:handle_request",
                GraphEdgeType::Calls,
                "open-kioku-resolution",
                EvidenceSourceType::Scip,
                Confidence::Exact,
            ),
            (
                "imports-config",
                "file:app",
                "file:config",
                GraphEdgeType::Imports,
                "open-kioku-import-resolver/rust",
                EvidenceSourceType::StaticAnalysis,
                Confidence::High,
            ),
        ] {
            store.edges.push(evidence_edge(
                id,
                from,
                to,
                edge_type,
                source,
                source_type,
                confidence,
            ));
        }
        store
    }

    fn file_node(id: &str, path: &str) -> open_kioku_core::GraphNode {
        open_kioku_core::GraphNode {
            file_id: Some(FileId::new(id)),
            ..test_node(id, path, GraphNodeType::File)
        }
    }

    /// A node built from an indexed symbol, carrying the file id of the File node `file`.
    fn symbol_node(
        id: &str,
        label: &str,
        node_type: GraphNodeType,
        file: &str,
    ) -> open_kioku_core::GraphNode {
        open_kioku_core::GraphNode {
            file_id: Some(FileId::new(file)),
            symbol_id: Some(open_kioku_core::SymbolId::new(id)),
            ..test_node(id, label, node_type)
        }
    }

    fn evidence_edge(
        id: &str,
        from: &str,
        to: &str,
        edge_type: GraphEdgeType,
        source: &str,
        source_type: EvidenceSourceType,
        confidence: Confidence,
    ) -> open_kioku_core::GraphEdge {
        let mut edge = test_edge(id, from, to, edge_type);
        edge.evidence.source = source.into();
        edge.evidence.source_type = source_type;
        edge.evidence.confidence = confidence;
        edge
    }

    fn run(store: &dyn open_kioku_storage::GraphStore, query: &str) -> GraphQueryResult {
        let ast = parse_graph_query(query)
            .unwrap_or_else(|error| panic!("{query} does not parse: {error}"));
        execute_graph_query(store, &ast, GraphQueryOptions::default())
            .unwrap_or_else(|error| panic!("{query} does not run: {error}"))
    }

    /// The node ids in one RETURN column, sorted, since the mock store iterates a HashMap.
    fn column_ids(result: &GraphQueryResult, column: usize) -> Vec<String> {
        let mut ids = result
            .rows
            .iter()
            .map(|row| row[column]["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn parse_error(query: &str) -> String {
        parse_graph_query(query).unwrap_err().to_string()
    }

    #[test]
    fn a_filter_field_the_variable_type_does_not_take_lists_the_fields_it_takes() {
        assert_eq!(
            parse_error(
                "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.protocol = 'http' RETURN f"
            ),
            "Parse error: Unknown filter field: f.protocol; File nodes filter on label, id, file_path"
        );
        assert_eq!(
            parse_error(
                "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.qualified_name = 'src::config' RETURN f"
            ),
            "Parse error: Unknown filter field: f.qualified_name; File nodes filter on label, id, file_path"
        );
        assert_eq!(
            parse_error(
                "MATCH (s:Function)-[:READS_CONFIG]->(k:ConfigKey) WHERE k.file_path = 'src/config.rs' RETURN s"
            ),
            "Parse error: Unknown filter field: k.file_path; ConfigKey nodes filter on label, id"
        );
        assert_eq!(
            parse_error(
                "MATCH (f:File)-[:DEFINES]->(m:Method) WHERE m.protocol = 'http' RETURN m"
            ),
            "Parse error: Unknown filter field: m.protocol; Method nodes filter on label, id, file_path, qualified_name"
        );
        assert_eq!(
            parse_error("MATCH (a)-[:CALLS]->(b) WHERE b.protocol = 'http' RETURN a"),
            "Parse error: Unknown filter field: b.protocol; nodes filter on label, id, file_path, qualified_name"
        );
    }

    #[test]
    fn evidence_fields_on_a_node_say_to_bind_the_edge() {
        for field in ["evidence_source", "evidence_source_type", "confidence"] {
            assert_eq!(
                parse_error(&format!(
                    "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE b.{field} = 'high' RETURN a"
                )),
                format!(
                    "Parse error: Unknown filter field: b.{field}; Function nodes filter on label, id, \
                     file_path, qualified_name; {field} is read from edge evidence: bind the edge as \
                     -[e:TYPE]-> and filter e.{field}"
                )
            );
        }
    }

    #[test]
    fn node_fields_on_an_edge_variable_list_the_edge_fields() {
        assert_eq!(
            parse_error("MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.label = 'x' RETURN a"),
            "Parse error: Unknown filter field: c.label; edges filter on evidence_source, \
             evidence_source_type, confidence; label is a node field: filter it on a node variable"
        );
        assert_eq!(
            parse_error(
                "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.protocol = 'http' RETURN a"
            ),
            "Parse error: Unknown filter field: c.protocol; edges filter on evidence_source, \
             evidence_source_type, confidence"
        );
        // An edge does have an id, so saying "id is a node field" would be wrong advice.
        assert_eq!(
            parse_error("MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.id = 'e1' RETURN a"),
            "Parse error: Unknown filter field: c.id; edges filter on evidence_source, \
             evidence_source_type, confidence; an edge has an id, but only its evidence fields \
             can be filtered"
        );
    }

    // In Cypher an edge's "source" is the node it leaves, so `source` and `source_type` read as the
    // endpoint and its type. Neither is a field; the parse error says how each reading is written.
    #[test]
    fn source_and_source_type_name_the_endpoint_reading_and_the_evidence_field() {
        let source_hint = "; an edge's source node is the node variable on its left: filter its \
                           label or id. The pass that recorded an edge's evidence is \
                           evidence_source on a bound edge";
        let source_type_hint = "; an edge's source node type is written in the pattern, such as \
                                (a:Function). The kind of evidence behind an edge is \
                                evidence_source_type on a bound edge";
        let edge_fields = "edges filter on evidence_source, evidence_source_type, confidence";
        assert_eq!(
            parse_error(
                "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.source = 'auth' RETURN b"
            ),
            format!("Parse error: Unknown filter field: c.source; {edge_fields}{source_hint}")
        );
        assert_eq!(
            parse_error(
                "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.source_type = 'scip' RETURN b"
            ),
            format!(
                "Parse error: Unknown filter field: c.source_type; {edge_fields}{source_type_hint}"
            )
        );
        // On a node variable the old names get the same explanation, not the bind-the-edge hint,
        // which would name a field the edge does not take.
        let node_fields = "Function nodes filter on label, id, file_path, qualified_name";
        assert_eq!(
            parse_error("MATCH (a:Function)-[:CALLS]->(b:Function) WHERE a.source = 'x' RETURN b"),
            format!("Parse error: Unknown filter field: a.source; {node_fields}{source_hint}")
        );
        assert_eq!(
            parse_error(
                "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE a.source_type = 'x' RETURN b"
            ),
            format!(
                "Parse error: Unknown filter field: a.source_type; {node_fields}{source_type_hint}"
            )
        );
    }

    // `evidence_source` names the recording pass. A node label here used to parse, match nothing,
    // and read as an authoritative "nothing links these".
    #[test]
    fn a_node_named_as_source_is_rejected_rather_than_matching_nothing() {
        for value in [
            "src::config::parse_config",
            "file:src/config.rs",
            "symbol:abc123",
        ] {
            let error = parse_error(&format!(
                "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.evidence_source = '{value}' RETURN a"
            ));
            assert_eq!(
                error,
                format!(
                    "Parse error: {value} names a node, but evidence_source is the pass that \
                     recorded the edge's evidence, such as open-kioku-graph or \
                     open-kioku-resolution; to filter an endpoint, filter a node variable's label \
                     or id"
                ),
                "{value}"
            );
        }
        // A real pass name still parses.
        assert!(parse_graph_query(
            "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.evidence_source = 'open-kioku-resolution' RETURN a"
        )
        .is_ok());
    }

    #[test]
    fn comparisons_and_numbers_apply_to_confidence_only() {
        let calls = "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE";
        assert_eq!(
            parse_error(&format!("{calls} b.label >= 1 RETURN a")),
            "Parse error: `>=` compares numbers and applies to confidence only; label takes =, \
             STARTS_WITH or =~"
        );
        assert_eq!(
            parse_error(&format!("{calls} b.label = 1 RETURN a")),
            "Parse error: label takes a quoted string; unquoted numbers apply to confidence only"
        );
        assert_eq!(
            parse_error(&format!("{calls} c.confidence >= 'high' RETURN a")),
            "Parse error: `>=` compares numbers; write the value unquoted, such as \
             c.confidence >= 0.85"
        );
        assert_eq!(
            parse_error(&format!("{calls} c.confidence STARTS_WITH 'h' RETURN a")),
            "Parse error: confidence takes =, <, <=, > or >=, not STARTS_WITH; compare it with a \
             band such as 'high' or a number such as 0.85"
        );
        assert_eq!(
            parse_error(&format!("{calls} c.confidence = 'certain' RETURN a")),
            "Parse error: Unknown confidence: certain; confidence is low, medium, high, exact, or \
             an unquoted number such as 0.85"
        );
        let source_type = parse_error(&format!(
            "{calls} c.evidence_source_type = 'treesitter' RETURN a"
        ));
        assert!(
            source_type.starts_with(
                "Parse error: Unknown evidence_source_type: treesitter; evidence source types are \
                 tree_sitter, scip, "
            ),
            "{source_type}"
        );
    }

    #[test]
    fn an_edge_variable_binds_one_hop_for_where_only() {
        let ast = parse_graph_query(
            "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.confidence >= 0.9 RETURN a",
        )
        .unwrap();
        let PathExpr::OneHop { edge, .. } = &ast.match_clause.path else {
            panic!("a one-hop path was parsed as multi-hop");
        };
        assert_eq!(edge.variable.as_deref(), Some("c"));
        let filter = &ast.where_clause.as_ref().unwrap().filters[0];
        assert_eq!(filter.operator, FilterOperator::GreaterOrEqual);
        assert_eq!(filter.value, FilterValue::Number(0.9));

        assert_eq!(
            parse_error("MATCH (a:Function)-[c:CALLS]->(b:Function) RETURN a, c"),
            "Query rejected: returning edge variables is not supported: c; RETURN node variables \
             and filter the edge in WHERE"
        );
        assert_eq!(
            parse_error("MATCH (a:Function)-[c:CALLS *1..2]->(b:Function) RETURN b"),
            "Parse error: A hop range binds no single edge, so -[e:TYPE *min..max]-> cannot name \
             a variable; bind and filter one hop with -[e:TYPE]->"
        );
        assert_eq!(
            parse_error("MATCH (a:Function)-[a:CALLS]->(b:Function) RETURN b"),
            "Query rejected: variable a is bound to both a node and an edge"
        );
        assert_eq!(
            parse_error("MATCH (a:Function)-[CALLS]->(b:Function) RETURN b"),
            "Parse error: Write the edge type after a colon: -[:CALLS]->; -[CALLS]-> would bind a \
             variable named CALLS"
        );
    }

    #[test]
    fn numbers_tokenize_without_breaking_hop_ranges_or_arrows() {
        assert_eq!(
            tokenize("0.9 >= <= > < 1..3 <- ->").unwrap(),
            vec![
                Token::NumberLiteral("0.9".into()),
                Token::GreaterOrEqual,
                Token::LessOrEqual,
                Token::GreaterThan,
                Token::LessThan,
                Token::IntLiteral(1),
                Token::DotDot,
                Token::IntLiteral(3),
                Token::ArrowLeft,
                Token::ArrowRight,
            ]
        );
    }

    #[test]
    fn file_path_on_a_file_node_is_its_path() {
        let result = run(
            &example_store(),
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE f.file_path = 'src/config.rs' RETURN f, s",
        );
        assert_eq!(column_ids(&result, 0), ["file:config"]);
        assert_eq!(column_ids(&result, 1), ["fn:parse_config"]);
    }

    #[test]
    fn file_path_on_a_symbol_node_is_the_path_of_the_file_that_defines_it() {
        let store = example_store();
        let result = run(
            &store,
            "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE b.file_path = 'src/config.rs' RETURN a, b",
        );
        assert_eq!(column_ids(&result, 0), ["fn:run"]);
        assert_eq!(column_ids(&result, 1), ["fn:parse_config"]);
        assert_eq!(result.caveats.len(), 1, "{:?}", result.caveats);

        let java = run(
            &store,
            "MATCH (f:File)-[:DEFINES]->(m:Method) WHERE m.file_path = 'src/main/java/com/acme/OrderService.java' RETURN m",
        );
        assert_eq!(column_ids(&java, 0), ["method:handle_order"]);

        let untyped = run(
            &store,
            "MATCH (a)-[:CALLS]->(b) WHERE a.file_path = 'src/app.rs' RETURN b",
        );
        assert_eq!(column_ids(&untyped, 0), ["fn:parse_config"]);
    }

    // The regression in #449: file_path on a symbol node compared its label.
    #[test]
    fn a_symbol_label_no_longer_satisfies_file_path() {
        let store = example_store();
        let calls = "MATCH (a:Function)-[:CALLS]->(b:Function) WHERE";
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{calls} b.label = 'src::config::parse_config' RETURN b")
                ),
                0
            ),
            ["fn:parse_config"]
        );
        for filter in [
            "b.file_path = 'src::config::parse_config'",
            "b.file_path STARTS_WITH 'src::config'",
            "b.file_path =~ '::parse_config$'",
        ] {
            let result = run(&store, &format!("{calls} {filter} RETURN b"));
            assert!(result.rows.is_empty(), "{filter} matched {:?}", result.rows);
        }
    }

    #[test]
    fn qualified_name_on_a_symbol_node_is_its_label() {
        let store = example_store();
        let result = run(
            &store,
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.qualified_name = 'src::config::parse_config' RETURN f, s",
        );
        assert_eq!(column_ids(&result, 0), ["file:config"]);
        assert_eq!(column_ids(&result, 1), ["fn:parse_config"]);

        let java = run(
            &store,
            "MATCH (f:File)-[:DEFINES]->(m:Method) WHERE m.qualified_name =~ 'OrderService::handle_order$' RETURN m",
        );
        assert_eq!(column_ids(&java, 0), ["method:handle_order"]);

        let path = run(
            &store,
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.qualified_name = 'src/config.rs' RETURN s",
        );
        assert!(path.rows.is_empty(), "{:?}", path.rows);
    }

    #[test]
    fn nodes_that_carry_no_file_path_or_qualified_name_are_excluded_and_counted() {
        let mut store = example_store();
        // A Module node built from an import has no symbol and no file.
        store.nodes.insert(
            "module:config".into(),
            test_node("module:config", "crate::config", GraphNodeType::Module),
        );
        store.edges.push(test_edge(
            "imports-module",
            "file:app",
            "module:config",
            GraphEdgeType::Imports,
        ));
        // A symbol node that two File nodes define has no single path.
        store.nodes.insert(
            "fn:shared".into(),
            open_kioku_core::GraphNode {
                file_id: None,
                ..symbol_node(
                    "fn:shared",
                    "src::shared::shared",
                    GraphNodeType::Function,
                    "file:app",
                )
            },
        );
        for file in ["file:app", "file:config"] {
            store.edges.push(test_edge(
                &format!("defines-shared-{file}"),
                file,
                "fn:shared",
                GraphEdgeType::Defines,
            ));
        }

        let imports = "MATCH (f:File)-[:IMPORTS]->(m:Module) WHERE";
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{imports} m.label = 'crate::config' RETURN f")
                ),
                0
            ),
            ["file:app"]
        );
        let qualified = run(
            &store,
            &format!("{imports} m.qualified_name = 'crate::config' RETURN f"),
        );
        assert!(qualified.rows.is_empty(), "{:?}", qualified.rows);
        assert_eq!(
            qualified.caveats[1..],
            [
                "At least 1 scanned row(s) were excluded because the filtered node carries no \
                 qualified_name; the scan stops once the requested page is filled, so rows beyond \
                 it were not counted. qualified_name is carried by nodes built from an indexed \
                 symbol."
            ]
        );

        let defined = run(
            &store,
            "MATCH (f:File)-[:DEFINES]->(s:Function) WHERE s.file_path = 'src/app.rs' RETURN s",
        );
        assert_eq!(column_ids(&defined, 0), ["fn:handle_request", "fn:run"]);
        assert_eq!(
            defined.caveats[1..],
            [
                "At least 2 scanned row(s) were excluded because the filtered node carries no \
              file_path; the scan stops once the requested page is filled, so rows beyond it were \
              not counted. file_path is carried by File nodes and by symbol nodes that exactly one \
              File node DEFINES."
            ]
        );
    }

    #[test]
    fn evidence_source_and_evidence_source_type_read_the_bound_edge_evidence() {
        let store = example_store();
        let calls = "MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE";
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{calls} c.evidence_source = 'open-kioku-resolution' RETURN a")
                ),
                0
            ),
            ["fn:parse_config", "fn:run"]
        );
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{calls} c.evidence_source_type = 'scip' RETURN a")
                ),
                0
            ),
            ["fn:parse_config"]
        );
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{calls} c.evidence_source_type = 'tree_sitter' RETURN a")
                ),
                0
            ),
            ["fn:run"]
        );

        let defines = "MATCH (f:File)-[d:DEFINES]->(s:Function) WHERE";
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    &format!("{defines} d.evidence_source = 'open-kioku-graph' RETURN s")
                ),
                0
            ),
            ["fn:handle_request", "fn:parse_config", "fn:run"]
        );
        assert!(run(
            &store,
            &format!("{defines} d.evidence_source = 'open-kioku-resolution' RETURN s")
        )
        .rows
        .is_empty());
        assert_eq!(
            column_ids(
                &run(
                    &store,
                    "MATCH (f:File)-[i:IMPORTS]->(g:File) WHERE i.evidence_source STARTS_WITH 'open-kioku-import-resolver/' RETURN f"
                ),
                0
            ),
            ["file:app"]
        );
    }

    #[test]
    fn confidence_compares_the_band_score_as_a_number() {
        let store = example_store();
        // calls-parse from run is high (0.85); calls-handle from parse_config is exact (1.0).
        for (filter, callers) in [
            ("c.confidence >= 0.9", vec!["fn:parse_config"]),
            ("c.confidence < 0.9", vec!["fn:run"]),
            // As text, "0.85" sorts before "0.850" and the high edge would be dropped.
            ("c.confidence >= 0.850", vec!["fn:parse_config", "fn:run"]),
            // Widened to f64, the high band's 0.85 would sit just above 0.85.
            ("c.confidence <= 0.85", vec!["fn:run"]),
            ("c.confidence = 1", vec!["fn:parse_config"]),
            ("c.confidence = 'HIGH'", vec!["fn:run"]),
            ("c.confidence > 1.0", vec![]),
        ] {
            let result = run(
                &store,
                &format!("MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE {filter} RETURN a"),
            );
            assert_eq!(column_ids(&result, 0), callers, "{filter}");
        }
    }

    #[test]
    fn functions_distinct_and_aliases_in_return_say_return_accepts_only_variables() {
        for query in [
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN count(s)",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN DISTINCT s",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s AS symbol",
        ] {
            assert_eq!(
                parse_graph_query(query).unwrap_err().to_string(),
                "Parse error: RETURN accepts only variables bound in MATCH; functions, DISTINCT \
                 and AS aliases are not supported",
                "{query}"
            );
        }
    }

    #[test]
    fn unknown_node_type_lists_the_node_types_the_schema_names() {
        let error = parse_graph_query("MATCH (s:Symbol) RETURN s")
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "Parse error: Unknown node type: Symbol; node types are File, Directory, Module, \
             Package, Class, Trait, Interface, Function, Method, Field, Endpoint, DatabaseTable, \
             Collection, Queue, Topic, ConfigKey, Test, BuildTarget, RuntimeError, Ticket, \
             PullRequest, Resource, ArchitectureComponent"
        );
    }

    #[test]
    fn unknown_edge_type_lists_the_edge_types_the_schema_names() {
        let error = parse_graph_query("MATCH (f:File)-[:USES]->(p:Package) RETURN f")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with(
                "Parse error: Unknown edge type: USES; edge types are Contains, Defines, "
            ),
            "{error}"
        );
        assert!(error.contains(", DependsOn, "), "{error}");
        assert!(error.ends_with(", DerivedFrom"), "{error}");
    }

    #[test]
    fn match_without_an_edge_pattern_names_the_accepted_form() {
        for query in ["MATCH (f:File) RETURN f", "MATCH (f:File)"] {
            assert_eq!(
                parse_graph_query(query).unwrap_err().to_string(),
                "Parse error: MATCH needs an edge pattern such as \
                 (a:File)-[:DEFINES]->(b:Function); isolated node patterns are not supported",
                "{query}"
            );
        }
    }

    #[test]
    fn property_access_in_return_says_to_filter_in_where() {
        for query in [
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f.file_path",
            "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s.label",
        ] {
            assert_eq!(
                parse_graph_query(query).unwrap_err().to_string(),
                "Parse error: RETURN accepts variables only; filter properties in WHERE",
                "{query}"
            );
        }
    }

    #[test]
    fn trailing_token_error_names_the_token_and_its_column() {
        let query = "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN s ORDER BY s";
        let column = query.find("ORDER").unwrap() + 1;
        assert_eq!(
            parse_graph_query(query).unwrap_err().to_string(),
            format!(
                "Parse error: Unexpected token `ORDER` at column {column}; \
                 only LIMIT <n> and OFFSET <n> may follow RETURN"
            )
        );
    }

    #[test]
    fn write_like_keywords_are_rejected_naming_the_keyword() {
        for keyword in [
            "CREATE", "MERGE", "DELETE", "DETACH", "SET", "REMOVE", "DROP", "CALL", "LOAD",
            "UNION", "WITH", "FOREACH",
        ] {
            let query = format!("MATCH (f:File)-[:DEFINES]->(s:Function) {keyword} s RETURN s");
            assert_eq!(
                parse_graph_query(&query).unwrap_err().to_string(),
                format!("Parse error: Write-like or unsupported keyword rejected: {keyword}")
            );
        }
    }

    #[test]
    fn every_type_parses_as_the_schema_writes_it_and_as_it_serializes() {
        let schema = crate::schema::current_schema(None);
        assert_eq!(schema.node_types.len(), NODE_TYPES.len());
        assert_eq!(schema.edge_types.len(), EDGE_TYPES.len());

        for (spec, node_type) in schema.node_types.iter().zip(NODE_TYPES) {
            let serialized = crate::schema::node_type_query_spelling(&node_type);
            for spelling in [
                spec.name.clone(),
                spec.name.to_ascii_uppercase(),
                serialized.clone(),
                serialized.to_ascii_uppercase(),
            ] {
                let ast =
                    parse_graph_query(&format!("MATCH (a:{spelling})-[:DEFINES]->(b) RETURN a"))
                        .unwrap_or_else(|error| panic!("{spelling} does not parse: {error}"));
                let PathExpr::OneHop { source, .. } = ast.match_clause.path else {
                    panic!("a one-hop path was parsed as multi-hop");
                };
                assert_eq!(source.node_type.as_ref(), Some(&node_type), "{spelling}");
            }
        }

        for (spec, edge_type) in schema.edge_types.iter().zip(EDGE_TYPES) {
            let serialized = crate::schema::edge_type_query_spelling(&edge_type);
            for spelling in [
                spec.name.clone(),
                spec.name.to_ascii_lowercase(),
                serialized.clone(),
                serialized.to_ascii_lowercase(),
            ] {
                let ast = parse_graph_query(&format!("MATCH (a)-[:{spelling}]->(b) RETURN a"))
                    .unwrap_or_else(|error| panic!("{spelling} does not parse: {error}"));
                let PathExpr::OneHop { edge, .. } = ast.match_clause.path else {
                    panic!("a one-hop path was parsed as multi-hop");
                };
                assert_eq!(edge.edge_type.as_ref(), Some(&edge_type), "{spelling}");
            }
        }
    }

    #[test]
    fn every_schema_example_parses_and_returns_rows() {
        let store = example_store();
        let examples = crate::schema::current_schema(None).examples;
        assert!(examples.len() >= 4);
        for example in examples {
            let ast = parse_graph_query(&example.query)
                .unwrap_or_else(|error| panic!("{} does not parse: {error}", example.query));
            let result = execute_graph_query(&store, &ast, GraphQueryOptions::default())
                .unwrap_or_else(|error| panic!("{} does not run: {error}", example.query));
            assert!(
                !result.rows.is_empty(),
                "{} returned no rows from the example graph",
                example.query
            );
        }
    }

    #[test]
    fn every_unsupported_form_in_the_schema_is_rejected() {
        let store = example_store();
        for entry in crate::schema::current_schema(None).unsupported {
            let rejected = match parse_graph_query(&entry.example) {
                Err(_) => true,
                Ok(ast) => execute_graph_query(&store, &ast, GraphQueryOptions::default()).is_err(),
            };
            assert!(
                rejected,
                "{} is listed as unsupported but runs: {}",
                entry.form, entry.example
            );
        }
    }
}
