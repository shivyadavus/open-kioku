import re

with open('crates/open-kioku-graph/src/query.rs', 'r') as f:
    content = f.read()

# Replace thiserror derives
content = content.replace(
    """#[derive(Debug, thiserror::Error)]
pub enum GraphQueryError {
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Query rejected: {0}")]
    QueryRejected(String),
    #[error("Unknown node type: {0}")]
    UnknownNodeType(String),
    #[error("Unknown edge type: {0}")]
    UnknownEdgeType(String),
    #[error("Unsupported filter field: {0}")]
    UnsupportedFilter(String),
    #[error("Query execution timed out")]
    Timeout,
    #[error("Max depth exceeded: {0}")]
    DepthLimitExceeded(usize),
    #[error("Storage error: {0}")]
    Storage(#[from] OkError),
    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),
}""",
    """#[derive(Debug)]
pub enum GraphQueryError {
    ParseError(String),
    QueryRejected(String),
    UnknownNodeType(String),
    UnknownEdgeType(String),
    UnsupportedFilter(String),
    Timeout,
    DepthLimitExceeded(usize),
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
}"""
)

# Fix store.edges_between
content = content.replace("store.edges_between", "store.graph_edges_between")

with open('crates/open-kioku-graph/src/query.rs', 'w') as f:
    f.write(content)
