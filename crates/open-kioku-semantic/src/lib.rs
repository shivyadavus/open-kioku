use open_kioku_errors::{OkError, Result};

pub fn ensure_enabled() -> Result<()> {
    Err(OkError::Unsupported(
        "semantic search requires an embedding provider; use open-kioku-embeddings".into(),
    ))
}
