use open_kioku_errors::{OkError, Result};

pub async fn run() -> Result<()> {
    Err(OkError::Unsupported(
        "daemon mode is reserved for long-running indexing and watch services".into(),
    ))
}
