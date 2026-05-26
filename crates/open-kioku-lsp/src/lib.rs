use open_kioku_errors::{OkError, Result};

pub fn ensure_enabled() -> Result<()> {
    Err(OkError::Unsupported(
        "LSP fallback is an extension boundary and is disabled by default".into(),
    ))
}
