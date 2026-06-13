mod architecture_policy;

pub use architecture_policy::{
    load_architecture_policy, load_architecture_policy_from_path, ArchitecturePolicy,
    DependencyAction, DependencyRule, ExemptionRule, PolicyContext, PolicyLayer, PolicySource,
    PolicyVersion, PublicApiRule, Severity, CANONICAL_ARCHITECTURE_POLICY_PATH,
    COMPATIBILITY_ARCHITECTURE_POLICY_PATH,
};

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
    #[serde(default)]
    pub history: HistoryConfig,
    pub search: SearchConfig,
    #[serde(default)]
    pub ranking: RankingConfig,
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
                    ".git/**".into(),
                    "**/.git/**".into(),
                    "node_modules/**".into(),
                    "**/node_modules/**".into(),
                    "target/**".into(),
                    "**/target/**".into(),
                    "dist/**".into(),
                    "**/dist/**".into(),
                    "build/**".into(),
                    "**/build/**".into(),
                    ".venv/**".into(),
                    "**/.venv/**".into(),
                    ".ok/**".into(),
                    "**/.ok/**".into(),
                    "package-lock.json".into(),
                    "**/package-lock.json".into(),
                    "pnpm-lock.yaml".into(),
                    "**/pnpm-lock.yaml".into(),
                    "yarn.lock".into(),
                    "**/yarn.lock".into(),
                    "bun.lockb".into(),
                    "**/bun.lockb".into(),
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
                mode: ScipMode::Consume,
                auto_generate: false,
                allow_install: false,
                timeout_seconds: 300,
                paths: vec![
                    "index.scip".into(),
                    ".ok/indexes/go.scip".into(),
                    ".ok/indexes/java.scip".into(),
                    ".ok/indexes/typescript.scip".into(),
                    ".ok/indexes/python.scip".into(),
                ],
            },
            history: HistoryConfig::default(),
            search: SearchConfig {
                lexical: "tantivy".into(),
                semantic: "disabled".into(),
                structural: true,
            },
            ranking: RankingConfig::default(),
            semantic: SemanticConfig {
                enabled: false,
                backend: "exact-flat".into(),
                provider: "local".into(),
                model: "local-hash".into(),
                dimensions: 384,
                distance: "cosine".into(),
                batch_size: 64,
                index_symbols: true,
                index_chunks: true,
                index_docs: true,
                index_memory: true,
                external_provider_allowed: false,
            },
            mcp: McpConfig {
                mode: "read-only".into(),
                transport: "stdio".into(),
                allow_write: false,
                hide_experimental: false,
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
                rules: default_architecture_rules(),
                policy: None,
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
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: ScipMode,
    #[serde(default)]
    pub auto_generate: bool,
    #[serde(default)]
    pub allow_install: bool,
    #[serde(default = "default_scip_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_history_max_commits")]
    pub max_commits: usize,
    #[serde(default = "default_history_max_files_per_commit")]
    pub max_files_per_commit: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_commits: 500,
            max_files_per_commit: 40,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScipMode {
    Off,
    #[default]
    Consume,
    Auto,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub lexical: String,
    pub semantic: String,
    pub structural: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingConfig {
    pub text_relevance: f32,
    pub exact_reference: f32,
    pub graph_proximity: f32,
    pub boundary_fit: f32,
    pub runtime_corroboration: f32,
    pub git_cochange: f32,
    pub validation_proximity: f32,
    pub memory_signal: f32,
    pub path_quality: f32,
    #[serde(default = "default_semantic_similarity_weight")]
    pub semantic_similarity: f32,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            text_relevance: 1.0,
            exact_reference: 1.0,
            graph_proximity: 0.35,
            boundary_fit: 0.25,
            runtime_corroboration: 0.30,
            git_cochange: 0.25,
            validation_proximity: 1.0,
            memory_signal: 0.20,
            path_quality: 1.0,
            semantic_similarity: default_semantic_similarity_weight(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_semantic_backend")]
    pub backend: String,
    #[serde(default = "default_semantic_provider")]
    pub provider: String,
    #[serde(default = "default_semantic_model")]
    pub model: String,
    #[serde(default = "default_semantic_dimensions")]
    pub dimensions: usize,
    #[serde(default = "default_semantic_distance")]
    pub distance: String,
    #[serde(default = "default_semantic_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_true")]
    pub index_symbols: bool,
    #[serde(default = "default_true")]
    pub index_chunks: bool,
    #[serde(default = "default_true")]
    pub index_docs: bool,
    #[serde(default = "default_true")]
    pub index_memory: bool,
    #[serde(default)]
    pub external_provider_allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mode: String,
    pub transport: String,
    pub allow_write: bool,
    #[serde(default)]
    pub hide_experimental: bool,
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
    #[serde(default = "default_architecture_rules")]
    pub rules: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ArchitecturePolicy>,
}

impl OkConfig {
    pub fn load_from_repo(repo: impl AsRef<Path>) -> Result<Self> {
        let repo = repo.as_ref();
        let path = repo.join("ok.toml");
        if !path.exists() {
            let mut config = Self::default();
            config.architecture.policy = load_architecture_policy(repo)?;
            config.validate()?;
            return Ok(config);
        }
        let raw = fs::read_to_string(&path)?;
        let mut config: Self = toml::from_str(&raw)
            .map_err(|err| OkError::Config(format!("{}: {err}", path.display())))?;
        config.apply_builtin_excludes();
        config.normalize_scip();
        config.apply_env_overrides();
        config.architecture.policy = load_architecture_policy(repo)?;
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
        if self.scip.allow_install && self.security.deny_network {
            return Err(OkError::Config(
                "scip.allow_install cannot be true while security.deny_network is true".into(),
            ));
        }
        if self.semantic.provider == "external" && !self.semantic.external_provider_allowed {
            return Err(OkError::Config(
                "semantic external providers require semantic.external_provider_allowed = true"
                    .into(),
            ));
        }
        if self.semantic.dimensions == 0 {
            return Err(OkError::Config(
                "semantic.dimensions must be greater than zero".into(),
            ));
        }
        if let Some(policy) = &self.architecture.policy {
            policy.validate(COMPATIBILITY_ARCHITECTURE_POLICY_PATH)?;
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

    fn apply_builtin_excludes(&mut self) {
        for pattern in [
            ".git/**",
            "node_modules/**",
            "target/**",
            "dist/**",
            "build/**",
            ".venv/**",
            ".ok/**",
            "package-lock.json",
            "**/package-lock.json",
            "pnpm-lock.yaml",
            "**/pnpm-lock.yaml",
            "yarn.lock",
            "**/yarn.lock",
            "bun.lockb",
            "**/bun.lockb",
        ] {
            if !self
                .index
                .exclude
                .iter()
                .any(|existing| existing == pattern)
            {
                self.index.exclude.push(pattern.into());
            }
        }
    }

    fn normalize_scip(&mut self) {
        if !self.scip.enabled {
            self.scip.mode = ScipMode::Off;
        } else if self.scip.auto_generate && self.scip.mode == ScipMode::Consume {
            self.scip.mode = ScipMode::Auto;
        }
        for path in [
            "index.scip",
            ".ok/indexes/go.scip",
            ".ok/indexes/java.scip",
            ".ok/indexes/typescript.scip",
            ".ok/indexes/python.scip",
        ] {
            let path = PathBuf::from(path);
            if !self.scip.paths.iter().any(|existing| existing == &path) {
                self.scip.paths.push(path);
            }
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_history_max_commits() -> usize {
    500
}

fn default_history_max_files_per_commit() -> usize {
    40
}

fn default_scip_timeout_seconds() -> u64 {
    300
}

fn default_semantic_similarity_weight() -> f32 {
    0.30
}

fn default_semantic_backend() -> String {
    "exact-flat".into()
}

fn default_semantic_provider() -> String {
    "local".into()
}

fn default_semantic_model() -> String {
    "local-hash".into()
}

fn default_semantic_dimensions() -> usize {
    384
}

fn default_semantic_distance() -> String {
    "cosine".into()
}

fn default_semantic_batch_size() -> usize {
    64
}

fn default_architecture_rules() -> PathBuf {
    ".ok/architecture-rules.yml".into()
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
    use super::ScipMode;
    use super::{parse_size, ArchitectureConfig, OkConfig, PolicySource};
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
        assert_eq!(config.scip.mode, ScipMode::Consume);
        assert!(config
            .scip
            .paths
            .iter()
            .any(|path| path == std::path::Path::new("index.scip")));
    }

    #[test]
    fn embedded_policy_preserves_legacy_rules_default() {
        let architecture: ArchitectureConfig = toml::from_str(
            r#"
[policy]
version = "v1"

[[policy.layers]]
id = "api"
paths = ["crates/api/**"]
"#,
        )
        .unwrap();

        assert_eq!(
            architecture.rules,
            std::path::Path::new(".ok/architecture-rules.yml")
        );
        assert!(architecture.policy.is_some());
    }

    #[test]
    fn repo_config_exposes_canonical_architecture_policy() {
        let dir = tempfile::tempdir().unwrap();
        OkConfig::write_default(dir.path().join("ok.toml")).unwrap();
        let policy_path = dir.path().join(".open-kioku/architecture.toml");
        std::fs::create_dir_all(policy_path.parent().unwrap()).unwrap();
        std::fs::write(
            policy_path,
            include_str!("../../../examples/architecture-policy.toml"),
        )
        .unwrap();

        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        let policy = loaded.architecture.policy.unwrap();
        assert_eq!(policy.source, PolicySource::Canonical);
        assert_eq!(policy.layers.len(), 3);
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
        assert_eq!(loaded.scip.mode, ScipMode::Consume);
    }

    #[test]
    fn auto_generate_upgrades_legacy_scip_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ok.toml"),
            r#"
[repo]
name = "legacy"
root = "."

[index]
incremental = true
max_file_size = "1mb"
exclude = []

[languages]
enabled = ["rust"]

[scip]
enabled = true
auto_generate = true
paths = []

[search]
lexical = "tantivy"
semantic = "disabled"
structural = true

[ranking]
text_relevance = 1.0
exact_reference = 1.0
graph_proximity = 0.35
boundary_fit = 0.25
runtime_corroboration = 0.30
git_cochange = 0.25
validation_proximity = 1.0
memory_signal = 0.20
path_quality = 1.0

[semantic]
enabled = false
provider = "local"
model = ""

[mcp]
mode = "read-only"
transport = "stdio"
allow_write = false
hide_experimental = false

[security]
redact_secrets = true
deny_network = true
allow_hidden_files = false
allow_write = false
approval_required = true

[commands]
allow = []

[paths]
deny = []

[architecture]
rules = ".ok/architecture-rules.yml"
"#,
        )
        .unwrap();
        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        assert_eq!(loaded.scip.mode, ScipMode::Auto);
        assert_eq!(loaded.ranking.text_relevance, 1.0);
        assert_eq!(loaded.ranking.graph_proximity, 0.35);
    }

    #[test]
    fn load_adds_root_dependency_excludes_to_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.toml");
        OkConfig::write_default(&path).unwrap();
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw = raw.replace("    \"node_modules/**\",\n", "");
        std::fs::write(&path, raw).unwrap();

        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();

        assert!(loaded
            .index
            .exclude
            .iter()
            .any(|pattern| pattern == "node_modules/**"));
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
