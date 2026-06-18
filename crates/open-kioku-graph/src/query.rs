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
    let tokens = tokenize(input)?;
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

    let start_time = Instant::now();
    let deadline = Duration::from_millis(options.deadline_ms);

    let mut warnings = Vec::new();
    if query.limit.unwrap_or(0) > HARD_ROW_LIMIT {
        warnings.push(format!("LIMIT clamped to {}", HARD_ROW_LIMIT));
    }

    let mut rows: Vec<HashMap<String, serde_json::Value>> = Vec::new();
    let MatchClause { path } = &query.match_clause;

    let check_node = |node: &open_kioku_core::GraphNode, expr: &NodeExpr| -> bool {
        if let Some(t) = &expr.node_type {
            if &node.node_type != t {
                return false;
            }
        }
        true
    };

    let apply_filters = |row: &HashMap<String, serde_json::Value>| -> QueryResult<bool> {
        let Some(where_clause) = &query.where_clause else {
            return Ok(true);
        };

        for filter in &where_clause.filters {
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
            let edges = if let Some(t) = &edge.edge_type {
                store.edges_by_type(t.clone(), limit * 10 + offset, 0)?
            } else {
                return Err(GraphQueryError::ParseError(
                    "OneHop path requires an edge type for initial candidate narrowing".into(),
                ));
            };

            for e in edges {
                if start_time.elapsed() > deadline {
                    return Err(GraphQueryError::Timeout);
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

                if apply_filters(&row)? {
                    rows.push(row);
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

            let start_nodes = if let Some(t) = &source.node_type {
                store.nodes_by_type(t.clone(), 1000, 0)?
            } else {
                return Err(GraphQueryError::ParseError(
                    "MultiHop requires a source node type for initial narrowing".into(),
                ));
            };

            for start_node in start_nodes {
                if start_time.elapsed() > deadline {
                    return Err(GraphQueryError::Timeout);
                }

                let mut queue = std::collections::VecDeque::new();
                let mut visited = std::collections::HashSet::new();

                queue.push_back((start_node.clone(), 0));

                while let Some((curr_node, depth)) = queue.pop_front() {
                    if !visited.insert(curr_node.id.0.clone()) {
                        continue;
                    }

                    if depth >= edge_range.min_hops && depth <= edge_range.max_hops {
                        if check_node(&curr_node, target) {
                            let mut row = HashMap::new();
                            if let Some(v) = &source.variable {
                                row.insert(v.clone(), serde_json::to_value(&start_node)?);
                            }
                            if let Some(v) = &target.variable {
                                row.insert(v.clone(), serde_json::to_value(&curr_node)?);
                            }
                            if apply_filters(&row)? {
                                rows.push(row.clone());
                            }
                        }
                    }

                    if depth < edge_range.max_hops {
                        let (_, edges) = store.neighbors(&curr_node.id.0, limit * 10 + offset)?;
                        for edge in edges {
                            let expected_from = match edge_range.direction {
                                Direction::Forward => &curr_node.id.0,
                                Direction::Reverse => &edge.to.0, // handled below by checking edge direction properly
                            };

                            // Follow only forward edges for multi-hop
                            if edge.from.0 != curr_node.id.0 {
                                continue;
                            }

                            if let Some(expected_type) = &edge_range.edge_type {
                                if &edge.edge_type != expected_type {
                                    continue;
                                }
                            }

                            if !visited.contains(&edge.to.0) {
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

    let has_more = rows.len() > offset + limit;
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
        caveats: vec!["Filters applied in-memory after indexed edge lookup.".into()],
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
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }

        match c {
            '(' => {
                tokens.push(Token::LParen);
                chars.next();
            }
            ')' => {
                tokens.push(Token::RParen);
                chars.next();
            }
            '[' => {
                tokens.push(Token::LBracket);
                chars.next();
            }
            ']' => {
                tokens.push(Token::RBracket);
                chars.next();
            }
            ':' => {
                tokens.push(Token::Colon);
                chars.next();
            }
            ',' => {
                tokens.push(Token::Comma);
                chars.next();
            }
            '*' => {
                tokens.push(Token::Asterisk);
                chars.next();
            }
            '=' => {
                chars.next();
                if let Some(&'~') = chars.peek() {
                    chars.next();
                    tokens.push(Token::RegexMatch);
                } else {
                    tokens.push(Token::Equals);
                }
            }
            '.' => {
                chars.next();
                if let Some(&'.') = chars.peek() {
                    chars.next();
                    tokens.push(Token::DotDot);
                } else {
                    tokens.push(Token::Dot);
                }
            }
            '-' => {
                chars.next();
                if let Some(&'>') = chars.peek() {
                    chars.next();
                    tokens.push(Token::ArrowRight);
                } else {
                    tokens.push(Token::Dash);
                }
            }
            '<' => {
                chars.next();
                if let Some(&'-') = chars.peek() {
                    chars.next();
                    tokens.push(Token::ArrowLeft);
                } else {
                    return Err(GraphQueryError::ParseError(format!(
                        "Unexpected character: <"
                    )));
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut string_lit = String::new();
                let mut closed = false;
                while let Some(&next_c) = chars.peek() {
                    if next_c == quote {
                        chars.next();
                        closed = true;
                        break;
                    }
                    string_lit.push(next_c);
                    chars.next();
                }
                if !closed {
                    return Err(GraphQueryError::ParseError(format!(
                        "Unclosed string literal"
                    )));
                }
                tokens.push(Token::StringLiteral(string_lit));
            }
            _ if c.is_ascii_digit() => {
                let mut num_str = String::new();
                while let Some(&next_c) = chars.peek() {
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
                tokens.push(Token::IntLiteral(val));
            }
            _ if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&next_c) = chars.peek() {
                    if next_c.is_ascii_alphanumeric() || next_c == '_' {
                        ident.push(next_c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let token = match ident.to_uppercase().as_str() {
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
                };
                tokens.push(token);
            }
            _ => {
                return Err(GraphQueryError::ParseError(format!(
                    "Unexpected character: {}",
                    c
                )));
            }
        }
    }

    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn consume(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
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
                        "Unexpected token: {:?}",
                        t
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

        if let Some(t) = self.peek() {
            match t {
                Token::Dash | Token::ArrowLeft => {
                    let is_reverse = match self.consume().unwrap() {
                        Token::ArrowLeft => true,
                        Token::Dash => false,
                        _ => unreachable!(),
                    };
                    self.expect(Token::LBracket)?;

                    let mut edge_type = None;
                    if let Some(Token::Colon) = self.peek() {
                        self.consume();
                        if let Some(Token::Identifier(s)) = self.peek() {
                            let s = s.clone();
                            self.consume();
                            let json_val = serde_json::Value::String(s.to_uppercase());
                            edge_type =
                                Some(serde_json::from_value::<GraphEdgeType>(json_val).map_err(
                                    |_| {
                                        GraphQueryError::ParseError(format!(
                                            "Unknown edge type: {}",
                                            s
                                        ))
                                    },
                                )?);
                        }
                    }

                    if let Some(Token::Asterisk) = self.peek() {
                        self.consume();
                        let min_hops = match self.consume() {
                            Some(Token::IntLiteral(n)) => *n,
                            _ => {
                                return Err(GraphQueryError::ParseError(
                                    "Expected integer min hops".into(),
                                ))
                            }
                        };
                        self.expect(Token::DotDot)?;
                        let max_hops = match self.consume() {
                            Some(Token::IntLiteral(n)) => *n,
                            _ => {
                                return Err(GraphQueryError::ParseError(
                                    "Expected integer max hops".into(),
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
                                "Reverse multi-hop not supported in v1".into(),
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
                    } else {
                        self.expect(Token::RBracket)?;
                        let direction = if is_reverse {
                            self.expect(Token::Dash)?;
                            Direction::Reverse
                        } else {
                            self.expect(Token::ArrowRight)?;
                            Direction::Forward
                        };

                        let target = self.parse_node()?;
                        return Ok(PathExpr::OneHop {
                            source,
                            edge: EdgeExpr {
                                direction,
                                edge_type,
                                variable: None,
                            },
                            target,
                        });
                    }
                }
                _ => return Err(GraphQueryError::ParseError("Expected edge".into())),
            }
        }

        Err(GraphQueryError::ParseError(
            "Isolated nodes not supported".into(),
        ))
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
                let json_val = serde_json::Value::String(s.to_lowercase());
                node_type = Some(serde_json::from_value::<GraphNodeType>(json_val).map_err(
                    |_| GraphQueryError::ParseError(format!("Unknown node type: {}", s)),
                )?);
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
        match self.consume() {
            Some(Token::Identifier(s)) => variables.push(s.clone()),
            _ => {
                return Err(GraphQueryError::ParseError(
                    "Expected identifier in RETURN".into(),
                ))
            }
        }
        while let Some(Token::Comma) = self.peek() {
            self.consume();
            match self.consume() {
                Some(Token::Identifier(s)) => variables.push(s.clone()),
                _ => {
                    return Err(GraphQueryError::ParseError(
                        "Expected identifier after comma".into(),
                    ))
                }
            }
        }
        Ok(ReturnClause { variables })
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
            source,
            edge,
            target,
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
            _limit: usize,
            _offset: usize,
        ) -> open_kioku_errors::Result<Vec<open_kioku_core::GraphNode>> {
            Ok(self
                .nodes
                .values()
                .filter(|n| n.node_type == node_type)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn test_execute_multi_hop_edge_direction_and_type() {
        let mut store = MockGraphStore {
            nodes: std::collections::HashMap::new(),
            edges: Vec::new(),
        };
        store.nodes.insert(
            "f1".into(),
            open_kioku_core::GraphNode {
                id: open_kioku_core::NodeId::new("f1"),
                node_type: GraphNodeType::Function,
                label: "A".into(),
                properties: std::collections::BTreeMap::new(),
                ..Default::default()
            },
        );
        store.nodes.insert(
            "f2".into(),
            open_kioku_core::GraphNode {
                id: open_kioku_core::NodeId::new("f2"),
                node_type: GraphNodeType::Function,
                label: "B".into(),
                properties: std::collections::BTreeMap::new(),
                ..Default::default()
            },
        );
        store.edges.push(open_kioku_core::GraphEdge {
            id: open_kioku_core::EdgeId::new("e1"),
            from: open_kioku_core::NodeId::new("f1"),
            to: open_kioku_core::NodeId::new("f2"),
            edge_type: GraphEdgeType::Calls,
            evidence: open_kioku_core::Evidence::default(),
            properties: std::collections::BTreeMap::new(),
            schema_version: None,
            source_pass: None,
            index_mode: None,
            extractor_version: None,
            ambiguity: vec![],
            quality_notes: vec![],
        });
        store.edges.push(open_kioku_core::GraphEdge {
            id: open_kioku_core::EdgeId::new("e2"),
            from: open_kioku_core::NodeId::new("f1"),
            to: open_kioku_core::NodeId::new("f2"),
            edge_type: GraphEdgeType::Imports,
            evidence: open_kioku_core::Evidence::default(),
            properties: std::collections::BTreeMap::new(),
            schema_version: None,
            source_pass: None,
            index_mode: None,
            extractor_version: None,
            ambiguity: vec![],
            quality_notes: vec![],
        });

        let query =
            parse_graph_query("MATCH (a:Function)-[:CALLS *1..2]->(b:Function) RETURN a, b")
                .unwrap();
        let res = execute_graph_query(
            &store as &dyn open_kioku_storage::GraphStore,
            &query,
            GraphQueryOptions::default(),
        )
        .unwrap();
        // Since we explicitly filtered edges by CALLS type and correct direction, it should return 1 row.
        assert_eq!(res.rows.len(), 1);
    }
}
