use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, OcfError>;

#[derive(Debug, thiserror::Error)]
pub enum OcfError {
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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl OcfError {
    pub fn user_message(&self) -> String {
        match self {
            Self::PolicyDenied(message) => {
                format!("Denied by Open Code Factory security policy: {message}")
            }
            _ => self.to_string(),
        }
    }
}
