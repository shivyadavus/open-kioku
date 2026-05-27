use open_kioku_errors::{OkError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerConfig {
    pub enabled: bool,
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: PathBuf::new(),
            args: Vec::new(),
        }
    }
}

pub fn ensure_enabled(config: &LspServerConfig) -> Result<()> {
    if !config.enabled {
        return Err(OkError::Unsupported(
            "LSP fallback is disabled by default".into(),
        ));
    }
    validate_server_config(config)
}

pub fn validate_server_config(config: &LspServerConfig) -> Result<()> {
    if config.command.as_os_str().is_empty() {
        return Err(OkError::Unsupported(
            "LSP server command must be configured before enabling LSP fallback".into(),
        ));
    }
    if config
        .args
        .iter()
        .any(|arg| arg.contains('\0') || arg.contains('\n'))
    {
        return Err(OkError::Unsupported(
            "LSP server arguments may not contain control characters".into(),
        ));
    }
    if config.command.is_absolute() && !is_executable_file(&config.command) {
        return Err(OkError::Unsupported(format!(
            "LSP server command is not an executable file: {}",
            config.command.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_lsp_returns_clear_error() {
        let err = ensure_enabled(&LspServerConfig::default()).unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn enabled_lsp_requires_command() {
        let config = LspServerConfig {
            enabled: true,
            ..LspServerConfig::default()
        };

        let err = ensure_enabled(&config).unwrap_err();
        assert!(err.to_string().contains("command"));
    }

    #[test]
    fn rejects_control_characters_in_args() {
        let config = LspServerConfig {
            enabled: true,
            command: PathBuf::from("rust-analyzer"),
            args: vec!["--stdio\n--bad".into()],
        };

        let err = ensure_enabled(&config).unwrap_err();
        assert!(err.to_string().contains("control"));
    }
}
