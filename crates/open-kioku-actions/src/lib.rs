use globset::{Glob, GlobSetBuilder};
use open_kioku_config::OcfConfig;
use open_kioku_errors::{OcfError, Result};
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
    config: &'a OcfConfig,
}

impl<'a> PolicyGate<'a> {
    pub fn new(config: &'a OcfConfig) -> Self {
        Self { config }
    }

    pub fn ensure_allowed(&self, action: ActionKind) -> Result<()> {
        match action {
            ActionKind::Read => Ok(()),
            ActionKind::WriteFile | ActionKind::ApplyPatch if !self.config.security.allow_write => {
                Err(OcfError::PolicyDenied("file writes are disabled".into()))
            }
            ActionKind::RunCommand => Err(OcfError::PolicyDenied(
                "shell execution requires explicit allowlisted command".into(),
            )),
            ActionKind::Network if self.config.security.deny_network => {
                Err(OcfError::PolicyDenied("network access is denied".into()))
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
            Err(OcfError::PolicyDenied(format!(
                "command `{command}` is not in the allowlist"
            )))
        }
    }

    pub fn ensure_path_readable(&self, path: &Path) -> Result<()> {
        // Use globset for correct glob matching rather than ad-hoc string ops.
        // This correctly handles patterns like `.env`, `.env.local`,
        // `.aws/**`, `**/secrets/**`, etc.
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
            .map_err(|err| OcfError::Config(err.to_string()))?;
        if set.is_match(value.as_ref()) {
            Err(OcfError::PolicyDenied(format!(
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
    use open_kioku_config::OcfConfig;

    fn config_write_allowed() -> OcfConfig {
        let mut config = OcfConfig::default();
        config.security.allow_write = true;
        config.security.deny_network = false;
        config.mcp.mode = "write".into();
        config
    }

    #[test]
    fn read_is_always_allowed() {
        let config = OcfConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::Read).is_ok());
    }

    #[test]
    fn write_denied_by_default() {
        let config = OcfConfig::default();
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
        let config = OcfConfig::default();
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_allowed(ActionKind::Network).is_err());
    }

    #[test]
    fn command_allowlist_exact_match() {
        let config = OcfConfig::default(); // allows "cargo test" etc.
        let gate = PolicyGate::new(&config);
        assert!(gate.ensure_command_allowed("cargo test").is_ok());
        assert!(gate.ensure_command_allowed("rm -rf /").is_err());
    }

    #[test]
    fn path_denial_uses_globset() {
        let config = OcfConfig::default(); // denies .env, .aws/**, .ssh/**, **/secrets/**
        let gate = PolicyGate::new(&config);
        use std::path::Path;
        // Exact deny
        assert!(gate.ensure_path_readable(Path::new(".env")).is_err());
        // Prefix glob
        assert!(gate
            .ensure_path_readable(Path::new(".aws/credentials"))
            .is_err());
        assert!(gate.ensure_path_readable(Path::new(".ssh/id_rsa")).is_err());
        // Recursive glob
        assert!(gate
            .ensure_path_readable(Path::new("infra/secrets/db.yaml"))
            .is_err());
        // Safe paths allowed
        assert!(gate.ensure_path_readable(Path::new("src/main.rs")).is_ok());
        assert!(gate.ensure_path_readable(Path::new("README.md")).is_ok());
    }
}
