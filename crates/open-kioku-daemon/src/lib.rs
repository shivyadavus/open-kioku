use open_kioku_errors::{OcfError, Result};

pub async fn run() -> Result<()> {
    Err(OcfError::Unsupported(
        "daemon mode is reserved for long-running indexing and watch services".into(),
    ))
}
