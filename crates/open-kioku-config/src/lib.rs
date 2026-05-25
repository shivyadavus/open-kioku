use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkConfig {
    pub repo: RepoConfig,
    pub index: IndexConfig,
    pub languages: LanguagesConfig,
    pub scip: ScipConfig,
    pub search: SearchConfig,
    pub semantic: SemanticConfig,
    pub mcp: McpConfig,
    pub security: SecurityConfig,
    pub commands: CommandsConfig,
    pub paths: PathsConfig,
    pub architecture: ArchitectureConfig,
}

impl Default for OkConfig {
    fn default() -> Self {
        Self {
            repo: RepoConfig {
                name: "open-kioku-repo".to_string(),
                root: PathBuf::from("."),
            },
            index: IndexConfig {
                incremental: true,
                max_file_size: "1mb".to_string(),
                exclude: vec![
                    "**/.git/**".into(),
                    "**/node_modules/**".into(),
                    "**/target/**".into(),
                    "**/dist/**".into(),
                    "**/build/**".into(),
                    "**/.venv/**".into(),
                    "**/.ok/**".into(),
                ],
            },
            languages: LanguagesConfig {
                enabled: vec![
                    "rust".into(),
                    "java".into(),
                    "typescript".into(),
                    "javascript".into(),
                    "python".into(),
                    "go".into(),
                    "yaml".into(),
                    "json".into(),
                    "toml".into(),
                    "sql".into(),
                ],
            },
            scip: ScipConfig {
                enabled: true,
                auto_generate: false,
                paths: vec![
                    ".ok/indexes/java.scip".into(),
                    ".ok/indexes/typescript.scip".into(),
                    ".ok/indexes/python.scip".into(),
                ],
            },
            search: SearchConfig {
                lexical: "tantivy".into(),
                semantic: "disabled".into(),
                structural: true,
            },
            semantic: SemanticConfig {
                enabled: false,
                provider: "local".into(),
                model: String::new(),
            },
            mcp: McpConfig {
                mode: "read-only".into(),
                transport: "stdio".into(),
                allow_write: false,
            },
            security: SecurityConfig {
                redact_secrets: true,
                deny_network: true,
                allow_hidden_files: false,
                allow_write: false,
                approval_required: true,
            },
            commands: CommandsConfig {
                allow: vec![
                    "cargo test".into(),
                    "cargo check".into(),
                    "mvn test".into(),
                    "npm test".into(),
                    "pytest".into(),
                ],
            },
            paths: PathsConfig {
                deny: vec![
                    ".env".into(),
                    ".aws/**".into(),
                    ".ssh/**".into(),
                    "**/secrets/**".into(),
                ],
            },
            architecture: ArchitectureConfig {
                rules: ".ok/architecture-rules.yml".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    pub incremental: bool,
    pub max_file_size: String,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguagesConfig {
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScipConfig {
    pub enabled: bool,
    pub auto_generate: bool,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub lexical: String,
    pub semantic: String,
    pub structural: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mode: String,
    pub transport: String,
    pub allow_write: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub redact_secrets: bool,
    pub deny_network: bool,
    pub allow_hidden_files: bool,
    pub allow_write: bool,
    pub approval_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandsConfig {
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureConfig {
    pub rules: PathBuf,
}

impl OkConfig {
    pub fn load_from_repo(repo: impl AsRef<Path>) -> Result<Self> {
        let path = repo.as_ref().join("ok.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let mut config: Self =
            toml::from_str(&raw).map_err(|err| OkError::Config(err.to_string()))?;
        config.apply_env_overrides();
        config.validate()?;
        Ok(config)
    }

    pub fn write_default(path: impl AsRef<Path>) -> Result<()> {
        let config = Self::default();
        let raw =
            toml::to_string_pretty(&config).map_err(|err| OkError::Config(err.to_string()))?;
        fs::write(path, raw)?;
        Ok(())
    }

    pub fn max_file_size_bytes(&self) -> Result<u64> {
        parse_size(&self.index.max_file_size)
    }

    pub fn validate(&self) -> Result<()> {
        if self.security.allow_write && self.mcp.mode == "read-only" {
            return Err(OkError::Config(
                "security.allow_write cannot be true while mcp.mode is read-only".into(),
            ));
        }
        if self.mcp.allow_write && !self.security.allow_write {
            return Err(OkError::Config(
                "mcp.allow_write cannot be true while security.allow_write is false".into(),
            ));
        }
        Ok(())
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(mode) = env::var("OK_SECURITY_MODE") {
            self.mcp.mode = mode.clone();
            self.security.allow_write = mode != "read-only";
        }
        if let Ok(value) = env::var("OK_DENY_NETWORK") {
            self.security.deny_network = value != "false";
        }
    }
}

pub fn parse_size(value: &str) -> Result<u64> {
    let trimmed = value.trim().to_ascii_lowercase();
    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(split_at);
    let number: u64 = digits
        .parse()
        .map_err(|_| OkError::Config(format!("invalid size: {value}")))?;
    let multiplier = match unit.trim() {
        "" | "b" => 1,
        "kb" | "kib" => 1024,
        "mb" | "mib" => 1024 * 1024,
        "gb" | "gib" => 1024 * 1024 * 1024,
        other => return Err(OkError::Config(format!("unsupported size unit: {other}"))),
    };
    Ok(number * multiplier)
}

#[cfg(test)]
mod tests {
    use super::{parse_size, OkConfig};
    use std::env;

    #[test]
    fn parses_human_size() {
        assert_eq!(parse_size("1mb").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("512kb").unwrap(), 512 * 1024);
        assert_eq!(parse_size("1gb").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("100b").unwrap(), 100);
        assert!(parse_size("badvalue").is_err());
    }

    #[test]
    fn default_config_is_valid() {
        let config = OkConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_catches_write_true_with_read_only_mcp() {
        let mut config = OkConfig::default();
        config.security.allow_write = true;
        config.mcp.mode = "read-only".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_catches_mcp_allow_write_without_security_allow_write() {
        let mut config = OkConfig::default();
        config.mcp.allow_write = true;
        config.security.allow_write = false;
        assert!(config.validate().is_err());
    }

    #[test]
    fn write_default_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.toml");
        OkConfig::write_default(&path).unwrap();
        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        assert_eq!(loaded.repo.name, "open-kioku-repo");
        assert!(!loaded.security.allow_write);
    }

    #[test]
    fn env_override_sets_read_only() {
        let dir = tempfile::tempdir().unwrap();
        OkConfig::write_default(dir.path().join("ok.toml")).unwrap();
        env::set_var("OK_SECURITY_MODE", "read-only");
        let config = OkConfig::load_from_repo(dir.path()).unwrap();
        assert_eq!(config.mcp.mode, "read-only");
        assert!(!config.security.allow_write);
        env::remove_var("OK_SECURITY_MODE");
    }

    #[test]
    fn max_file_size_bytes_parses_correctly() {
        let config = OkConfig::default();
        assert_eq!(config.max_file_size_bytes().unwrap(), 1024 * 1024);
    }
}
