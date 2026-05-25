use open_kioku_errors::{OcfError, Result};

pub fn ensure_enabled() -> Result<()> {
    Err(OcfError::Unsupported(
        "LSP fallback is an extension boundary and is disabled by default".into(),
    ))
}
