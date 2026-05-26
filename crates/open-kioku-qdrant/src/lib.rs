use open_kioku_errors::{OkError, Result};

pub fn ensure_configured() -> Result<()> {
    Err(OkError::Unsupported(
        "Qdrant integration is optional and not configured".into(),
    ))
}
