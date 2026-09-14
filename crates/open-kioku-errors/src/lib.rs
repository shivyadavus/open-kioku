use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, OkError>;

#[derive(Debug, thiserror::Error)]
pub enum OkError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("repository error: {0}")]
    Repository(String),
    #[error("index error: {0}")]
    Index(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("parse error in {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("search error: {0}")]
    Search(String),
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),
    #[error("operation denied by policy: {0}")]
    PolicyDenied(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    /// The caller's own arguments are wrong: a CLI usage error (exit code 2) or a JSON-RPC
    /// invalid-params error (-32602), never a configuration or repository problem.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl OkError {
    pub fn user_message(&self) -> String {
        match self {
            Self::PolicyDenied(message) => {
                format!("Denied by Open Kioku security policy: {message}")
            }
            _ => self.to_string(),
        }
    }

    /// Whether the failure is the caller's input rather than the tool's state.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self, Self::InvalidInput(_))
    }
}
