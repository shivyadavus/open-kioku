use crate::schema::{
    edge_type_for_query_name, edge_type_name, node_type_for_query_name, node_type_name, EDGE_TYPES,
    NODE_TYPES,
};
use open_kioku_core::{GraphEdgeType, GraphNodeType};
use open_kioku_errors::OkError;

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

fn validate_ast(ast: &GraphQueryAst) -> QueryResult<()> {
    let mut bound_vars = std::collections::HashSet::new();
    let mut edge_vars = std::collections::HashSet::new();

    match &ast.match_clause.path {
        PathExpr::OneHop {
            source,
            edge,
            target,
        } => {
            if let Some(v) = &source.variable {
                bound_vars.insert(v.clone());
            }
            if let Some(v) = &target.variable {
                bound_vars.insert(v.clone());
            }
            if let Some(v) = &edge.variable {
                bound_vars.insert(v.clone());
                edge_vars.insert(v.clone());
            }
        }
        PathExpr::MultiHop {
            source,
            edge_range,
            target,
        } => {
            if let Some(v) = &source.variable {
                bound_vars.insert(v.clone());
            }
            if let Some(v) = &target.variable {
                bound_vars.insert(v.clone());
            }
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
            if !bound_vars.contains(&filter.variable) {
                return Err(GraphQueryError::UnboundVariable(filter.variable.clone()));
            }
            if edge_vars.contains(&filter.variable) {
                return Err(GraphQueryError::QueryRejected(format!(
                    "filtering on edge variables is not supported: {}",
                    filter.variable
                )));
            }
        }
    }

    let mut returned_vars = std::collections::HashSet::new();
    for v in &ast.return_clause.variables {
        if !bound_vars.contains(v) {
            return Err(GraphQueryError::UnboundVariable(v.clone()));
        }
        if edge_vars.contains(v) {
            return Err(GraphQueryError::QueryRejected(format!(
                "returning edge variables is not supported: {}",
                v
            )));
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

    let mut rows: Vec<HashMap<String, serde_json::Value>> = Vec::new();
    let MatchClause { path } = &query.match_clause;
    let mut used_indexed_anchor = false;

    let check_node = |node: &open_kioku_core::GraphNode, expr: &NodeExpr| -> bool {
        if let Some(t) = &expr.node_type {
            if &node.node_type != t {
                return false;
            }
        }
        true
    };

    let apply_filters = |row: &HashMap<String, serde_json::Value>,
                         skip_filter: Option<&FilterExpr>|
     -> QueryResult<bool> {
        let Some(where_clause) = &query.where_clause else {
            return Ok(true);
        };

        for filter in &where_clause.filters {
            if skip_filter.is_some_and(|skip| std::ptr::eq(skip, filter)) {
                continue;
            }
            let val = match row.get(&filter.variable) {
                Some(v) => v,
                None => return Ok(false),
            };

            let field_val = match filter.field.as_str() {
                "label" => val.get("label").and_then(|v| v.as_str()),
                "file_path" => val.get("label").and_then(|v| v.as_str()),
                "qualified_name" => val.get("label").and_then(|v| v.as_str()),
                "id" => val.get("id").and_then(|v| v.as_str()),
                "source" => val.get("source").and_then(|v| v.as_str()),
                "source_type" => val.get("source_type").and_then(|v| v.as_str()),
                "confidence" => val.get("confidence").and_then(|v| v.as_str()),
                _ => None,
            };

            let field_val = match field_val {
                Some(s) => s,
                None => return Ok(false),
            };

            let matched = match filter.operator {
                FilterOperator::Equals => field_val == filter.value,
                FilterOperator::StartsWith => field_val.starts_with(&filter.value),
                FilterOperator::RegexMatch => {
                    if let Ok(re) = regex::Regex::new(&filter.value) {
                        re.is_match(field_val)
                    } else {
                        false
                    }
                }
            };

            if !matched {
                return Ok(false);
            }
        }

        Ok(true)
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
                    if filter.operator != FilterOperator::Equals
                        || !matches!(filter.field.as_str(), "id" | "label")
                    {
                        return None;
                    }
                    if source.variable.as_deref() == Some(filter.variable.as_str()) {
                        Some((true, filter))
                    } else if target.variable.as_deref() == Some(filter.variable.as_str()) {
                        Some((false, filter))
                    } else {
                        None
                    }
                })
            });

            let mut anchored_edges = Vec::new();
            if let Some((anchor_is_source, filter)) = anchor_filter {
                let anchor_expr = if anchor_is_source { source } else { target };
                let anchor_nodes = if filter.field == "id" {
                    match store.node_by_id(&filter.value) {
                        Ok(node) => Some(node.into_iter().collect::<Vec<_>>()),
                        Err(OkError::Unsupported(_)) => None,
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    let mut nodes = Vec::new();
                    let mut node_offset = 0;
                    loop {
                        match store.nodes_by_label(
                            &filter.value,
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
                            &filter.value,
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
                anchor_filter.map(|(_, filter)| filter)
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
                        row.insert(v.clone(), serde_json::to_value(&actual_source)?);
                    }
                    if let Some(v) = &target.variable {
                        row.insert(v.clone(), serde_json::to_value(&actual_target)?);
                    }
                    if let Some(v) = &edge.variable {
                        row.insert(v.clone(), serde_json::to_value(&e)?);
                    }

                    if apply_filters(&row, skip_anchor_filter)? {
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

                    let mut queue = std::collections::VecDeque::new();
                    let mut visited = std::collections::HashSet::new();

                    queue.push_back((start_node.clone(), 0));

                    while let Some((curr_node, depth)) = queue.pop_front() {
                        if rows.len() >= target_rows {
                            break;
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
                                row.insert(v.clone(), serde_json::to_value(&start_node)?);
                            }
                            if let Some(v) = &target.variable {
                                row.insert(v.clone(), serde_json::to_value(&curr_node)?);
                            }
                            if apply_filters(&row, None)? {
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
            out_row.push(row.get(col).cloned().unwrap_or(serde_json::Value::Null));
        }
        final_rows.push(serde_json::Value::Array(out_row));
    }

    let returned = final_rows.len();
    Ok(GraphQueryResult {
        columns,
        rows: final_rows,
        returned,
        limit,
        offset,
        has_more,
        warnings,
        caveats: vec![if used_indexed_anchor {
            "Equality filter anchored by indexed node and edge lookup.".into()
        } else {
            "Filters applied in-memory after indexed edge lookup.".into()
        }],
    })
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
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterOperator {
    Equals,
    StartsWith,
    RegexMatch,
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
                if let Some(&(_, '-')) = chars.peek() {
                    chars.next();
                    Token::ArrowLeft
                } else {
                    return Err(GraphQueryError::ParseError(
                        "Unexpected character: <".into(),
                    ));
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
                while let Some(&(_, next_c)) = chars.peek() {
                    if next_c.is_ascii_digit() {
                        num_str.push(next_c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let val: usize = num_str.parse().map_err(|_| {
                    GraphQueryError::ParseError(format!("Invalid integer: {}", num_str))
                })?;
                Token::IntLiteral(val)
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
                variable: None,
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

        let allowed_fields = [
            "label",
            "file_path",
            "qualified_name",
            "id",
            "source",
            "source_type",
            "confidence",
        ];
        if !allowed_fields.contains(&field.as_str()) {
            return Err(GraphQueryError::ParseError(format!(
                "Unsupported filter field: {}",
                field
            )));
        }

        let operator = match self.consume() {
            Some(Token::Equals) => FilterOperator::Equals,
            Some(Token::StartsWith) => FilterOperator::StartsWith,
            Some(Token::RegexMatch) => FilterOperator::RegexMatch,
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected =, STARTS_WITH, or =~".into(),
                ))
            }
        };

        let value = match self.consume() {
            Some(Token::StringLiteral(s)) => s.clone(),
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected string literal".into(),
                ))
            }
        };

        if operator == FilterOperator::RegexMatch {
            if !["label", "qualified_name", "file_path"].contains(&field.as_str()) {
                return Err(GraphQueryError::ParseError(
                    "Regex filter only allowed on label, qualified_name, and file_path".into(),
                ));
            }
            if value.len() > 100 {
                return Err(GraphQueryError::ParseError("Regex pattern too long".into()));
            }
            if regex::Regex::new(&value).is_err() {
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

    /// Labels are shaped as the graph builder writes them: a File node carries its
    /// repository-relative path and a symbol node its qualified name, computed here by the same
    /// `identity::qualified_name` the parser uses, so the examples are tested against real labels.
    fn example_store() -> MockGraphStore {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        let symbol_label = |path: &str, name: &str| {
            open_kioku_core::identity::qualified_name(
                std::path::Path::new(path),
                &open_kioku_core::Language::Rust,
                None,
                name,
            )
            .expect("a relative Rust path has a qualified name")
        };
        for (id, label, node_type) in [
            ("file:app", "src/app.rs".to_string(), GraphNodeType::File),
            (
                "file:config",
                "src/config.rs".to_string(),
                GraphNodeType::File,
            ),
            (
                "fn:run",
                symbol_label("src/app.rs", "run"),
                GraphNodeType::Function,
            ),
            (
                "fn:handle_request",
                symbol_label("src/app.rs", "handle_request"),
                GraphNodeType::Function,
            ),
            (
                "fn:parse_config",
                symbol_label("src/config.rs", "parse_config"),
                GraphNodeType::Function,
            ),
        ] {
            store
                .nodes
                .insert(id.into(), test_node(id, &label, node_type));
        }
        assert_eq!(
            store.nodes["fn:parse_config"].label,
            "src::config::parse_config"
        );

        for (id, from, to, edge_type) in [
            ("defines-run", "file:app", "fn:run", GraphEdgeType::Defines),
            (
                "defines-handle",
                "file:app",
                "fn:handle_request",
                GraphEdgeType::Defines,
            ),
            (
                "defines-parse",
                "file:config",
                "fn:parse_config",
                GraphEdgeType::Defines,
            ),
            (
                "calls-parse",
                "fn:run",
                "fn:parse_config",
                GraphEdgeType::Calls,
            ),
            (
                "calls-handle",
                "fn:parse_config",
                "fn:handle_request",
                GraphEdgeType::Calls,
            ),
            (
                "imports-config",
                "file:app",
                "file:config",
                GraphEdgeType::Imports,
            ),
        ] {
            store.edges.push(test_edge(id, from, to, edge_type));
        }
        store
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
