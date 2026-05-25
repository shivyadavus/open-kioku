use open_kioku_errors::{OcfError, Result};

pub fn ensure_configured() -> Result<()> {
    Err(OcfError::Unsupported("Sentry integration is optional and not configured".into()))
}

