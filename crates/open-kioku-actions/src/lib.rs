use globset::{Glob, GlobSetBuilder};
use open_kioku_config::OkConfig;
use open_kioku_errors::{OkError, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Read,
    WriteFile,
    ApplyPatch,
    RunCommand,
    Network,
}

pub struct PolicyGate<'a> {
    config: &'a OkConfig,
}

impl<'a> PolicyGate<'a> {
    pub fn new(config: &'a OkConfig) -> Self {
        Self { config }
    }

    pub fn ensure_allowed(&self, action: ActionKind) -> Result<()> {
        match action {
            ActionKind::Read => Ok(()),
            ActionKind::WriteFile | ActionKind::ApplyPatch if !self.config.security.allow_write => {
                Err(OkError::PolicyDenied("file writes are disabled".into()))
            }
            ActionKind::RunCommand => Err(OkError::PolicyDenied(
                "shell execution requires explicit allowlisted command".into(),
            )),
            ActionKind::Network if self.config.security.deny_network => {
                Err(OkError::PolicyDenied("network access is denied".into()))
            }
            _ => Ok(()),
        }
    }

    pub fn ensure_command_allowed(&self, command: &str) -> Result<()> {
        if self
            .config
            .commands
            .allow
            .iter()
            .any(|allowed| allowed == command)
        {
            Ok(())
        } else {
            Err(OkError::PolicyDenied(format!(
                "command `{command}` is not in the allowlist"
            )))
        }
    }

    pub fn ensure_path_readable(&self, path: &Path) -> Result<()> {
        let value = path.to_string_lossy();
        let mut builder = GlobSetBuilder::new();
        let mut any_pattern = false;
        for pattern in &self.config.paths.deny {
            if let Ok(glob) = Glob::new(pattern) {
                builder.add(glob);
                any_pattern = true;
            }
        }
        if !any_pattern {
            return Ok(());
        }
        let set = builder
            .build()
            .map_err(|err| OkError::Config(err.to_string()))?;
        if set.is_match(value.as_ref()) {
            Err(OkError::PolicyDenied(format!(
                "path `{}` is denied by configuration",
                path.display()
            )))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionKind, PolicyGate};
    use open_kioku_config::OkConfig;

    fn config_write_allowed() -> OkConfig {
        let mut config = OkConfig::default();
        config.security.allow_write = true;
        config.security.deny_network = false;
        config.mcp.mode = "write".into();
        config
    }

    #[test]
    fn read_is_always_allowed() {
        let config = OkConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::Read).is_ok());
    }

    #[test]
    fn write_denied_by_default() {
        let config = OkConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::WriteFile).is_err());
        assert!(gate.ensure_allowed(ActionKind::ApplyPatch).is_err());
    }

    #[test]
    fn write_allowed_when_configured() {
        let config = config_write_allowed();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::WriteFile).is_ok());
        assert!(gate.ensure_allowed(ActionKind::ApplyPatch).is_ok());
    }

    #[test]
    fn run_command_always_denied_via_ensure_allowed() {
        let config = config_write_allowed();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::RunCommand).is_err());
    }

    #[test]
    fn network_denied_by_default() {
        let config = OkConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::Network).is_err());
    }

    #[test]
    fn command_allowlist_exact_match() {
        let config = OkConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_command_allowed("cargo test").is_ok());
        assert!(gate.ensure_command_allowed("rm -rf /").is_err());
    }

    #[test]
    fn path_denial_uses_globset() {
        let config = OkConfig::default();
        let gate = PolicyGate::new(&config);
        use std::path::Path;
        assert!(gate.ensure_path_readable(Path::new(".env")).is_err());
        assert!(gate.ensure_path_readable(Path::new(".aws/credentials")).is_err());
        assert!(gate.ensure_path_readable(Path::new(".ssh/id_rsa")).is_err());
        assert!(gate.ensure_path_readable(Path::new("infra/secrets/db.yaml")).is_err());
        assert!(gate.ensure_path_readable(Path::new("src/main.rs")).is_ok());
        assert!(gate.ensure_path_readable(Path::new("README.md")).is_ok());
    }
}
