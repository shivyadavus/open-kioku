use open_kioku_errors::{OcfError, Result};
use std::path::Path;

pub fn watch_repo(_root: impl AsRef<Path>) -> Result<()> {
    Err(OcfError::Unsupported(
        "watch mode requires the daemon event loop; use open-kioku-daemon".into(),
    ))
}
