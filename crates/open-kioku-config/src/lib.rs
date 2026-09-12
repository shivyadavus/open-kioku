mod architecture_policy;

pub use architecture_policy::{
    load_architecture_policy, load_architecture_policy_from_path, ArchitecturePolicy,
    DependencyAction, DependencyRule, ExemptionRule, ExemptionScope, InternalOnlyRule,
    PolicyContext, PolicyLayer, PolicySource, PolicyVersion, PublicApiBoundaryRule, PublicApiRule,
    Severity, CANONICAL_ARCHITECTURE_POLICY_PATH, COMPATIBILITY_ARCHITECTURE_POLICY_PATH,
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
    #[serde(default)]
    pub documents: DocumentsConfig,
    pub languages: LanguagesConfig,
    pub scip: ScipConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    pub search: SearchConfig,
    #[serde(default)]
    pub ranking: RankingConfig,
    pub semantic: SemanticConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
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
                resolution_mode: ResolutionMode::Shadow,
            },
            documents: DocumentsConfig::default(),
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
                ann_min_rows: 10_000,
                index_symbols: true,
                index_chunks: true,
                index_docs: true,
                index_memory: true,
                external_provider_allowed: false,
            },
            memory: MemoryConfig::default(),
            runtime: RuntimeConfig::default(),
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
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub root: PathBuf,
}

/// How call, inheritance and type-use edges are resolved while indexing. The variant docs
/// are also the comment `ok init` writes above `resolution_mode` in `ok.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMode {
    /// Symbol-registry resolution only; the resolution quality report is skipped.
    Legacy,
    /// Runs the proof-gated resolver beside the registry, records its quality report and its
    /// proven edges, and keeps the registry's CALLS facts in the graph. The default.
    #[default]
    Shadow,
    /// The proof-gated resolver's proven CALLS edges replace the registry's.
    V2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    pub incremental: bool,
    pub max_file_size: String,
    pub exclude: Vec<String>,
    #[serde(default)]
    pub resolution_mode: ResolutionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Additional documentation-oriented plain-text paths. Markdown/MDX and README files are
    /// first-class and do not need to be listed here.
    #[serde(default)]
    pub plain_text: Vec<String>,
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            plain_text: Vec::new(),
        }
    }
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
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub text_relevance: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub exact_reference: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub graph_proximity: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub boundary_fit: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub runtime_corroboration: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub git_cochange: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub validation_proximity: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub memory_signal: f32,
    #[serde(serialize_with = "serialize_f32_decimal")]
    pub path_quality: f32,
    #[serde(
        default = "default_semantic_similarity_weight",
        serialize_with = "serialize_f32_decimal"
    )]
    pub semantic_similarity: f32,
}

/// Serde widens `f32` to `f64` on the way out, so a weight declared as `0.35` would be
/// written as `0.3499999940395355`. The f32's shortest round-trip decimal parses back to the
/// identical f32, so that is what goes on the wire.
fn serialize_f32_decimal<S: serde::Serializer>(
    value: &f32,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let decimal = value
        .to_string()
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(*value));
    serializer.serialize_f64(decimal)
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
    /// Minimum vector count at which the `auto` backend selects local HNSW.
    #[serde(default = "default_semantic_ann_min_rows")]
    pub ann_min_rows: usize,
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

/// Repository memory. The `.ok` memory store is always readable, so this does
/// not gate recall; it gates whether the memory tools are advertised on the
/// agent-facing MCP surface, which stays off until a repository opts in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// Runtime error integration. The runtime tools answer with an explicit
/// disabled response until a provider is configured, and advertising three
/// permanently inert names taught agents to reach for evidence that is not
/// there — so the surface stays quiet until this is configured.
///
/// The fields mirror what the provider itself requires; whether they add up to
/// a usable provider is the provider's judgement, not a flag set here, so that
/// `enabled = true` alone can never advertise a tool that cannot answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_runtime_provider")]
    pub provider: String,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_runtime_auth_token_env")]
    pub auth_token_env: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_runtime_provider(),
            organization: None,
            project: None,
            auth_token_env: default_runtime_auth_token_env(),
        }
    }
}

fn default_runtime_provider() -> String {
    "sentry".into()
}

fn default_runtime_auth_token_env() -> String {
    "SENTRY_AUTH_TOKEN".into()
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
}

impl OkConfig {
    pub fn load_from_repo(repo: impl AsRef<Path>) -> Result<Self> {
        let repo = repo.as_ref();
        let path = repo.join("ok.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)?;
        let mut config: Self = toml::from_str(&raw)
            .map_err(|err| OkError::Config(format!("{}: {err}", path.display())))?;
        config.apply_builtin_excludes();
        config.normalize_scip();
        config.apply_env_overrides();
        config.validate()?;
        Ok(config)
    }

    pub fn write_default(path: impl AsRef<Path>) -> Result<()> {
        fs::write(path, Self::default_toml()?)?;
        Ok(())
    }

    /// The `ok.toml` that `ok init` and `ok setup agent --apply` write: the serialized
    /// defaults, with the two adjustments serde cannot make on its own. `resolution_mode`
    /// carries the comment its enum docs give it, and `[runtime]` keeps only `enabled = false`
    /// with the provider fields shown as comments, so a file that says `provider = "sentry"`
    /// does not read as a configured integration. `load_from_repo` restores the omitted
    /// fields from their defaults.
    pub fn default_toml() -> Result<String> {
        let raw = toml::to_string_pretty(&Self::default())
            .map_err(|err| OkError::Config(err.to_string()))?;
        Ok(annotate_default_toml(&raw))
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
        if self.semantic.ann_min_rows == 0 {
            return Err(OkError::Config(
                "semantic.ann_min_rows must be greater than zero".into(),
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

fn default_semantic_ann_min_rows() -> usize {
    10_000
}

const RESOLUTION_MODE_COMMENT: &str = "\
# How call, inheritance and type-use edges are resolved while indexing:
#   \"legacy\": symbol-registry resolution only; the resolution quality report is skipped.
#   \"shadow\": runs the proof-gated resolver beside the registry, records its quality report
#             and its proven edges, and keeps the registry's CALLS facts in the graph.
#   \"v2\":     the proof-gated resolver's proven CALLS edges replace the registry's.
";

// Generated comments are indexed like any other TOML text, so they stay terse: every extra
// English word here is a lexical term a context pack can match against.
const RUNTIME_SECTION_COMMENT: &str = "\
# Runtime error provider; inert while enabled = false. Enabling it requires
# provider = \"sentry\", organization, project and auth_token_env = \"SENTRY_AUTH_TOKEN\".
";

/// Post-pass over the serialized defaults. It only edits lines the serializer is known to
/// emit for `OkConfig::default()`; a round-trip test holds it to producing a file the loader
/// reads back as the defaults.
fn annotate_default_toml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 1024);
    let mut in_runtime = false;
    for line in raw.lines() {
        if line.starts_with('[') {
            in_runtime = line == "[runtime]";
            if in_runtime {
                out.push_str(line);
                out.push('\n');
                out.push_str(RUNTIME_SECTION_COMMENT);
                continue;
            }
        }
        if in_runtime && (line.starts_with("provider = ") || line.starts_with("auth_token_env = "))
        {
            continue;
        }
        if line.starts_with("resolution_mode = ") {
            out.push_str(RESOLUTION_MODE_COMMENT);
        }
        out.push_str(line);
        out.push('\n');
    }
    out
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
    use super::{load_architecture_policy, parse_size, OkConfig, PolicySource};
    use super::{ResolutionMode, ScipMode};
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
        assert_eq!(config.index.resolution_mode, ResolutionMode::Shadow);
        assert_eq!(config.semantic.ann_min_rows, 10_000);
        assert!(config
            .scip
            .paths
            .iter()
            .any(|path| path == std::path::Path::new("index.scip")));
    }

    #[test]
    fn embedded_policy_preserves_full_config_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.toml");
        OkConfig::write_default(&path).unwrap();
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw = raw.replace("rules = \".ok/architecture-rules.yml\"\n", "");
        raw.push_str(
            r#"
[architecture.policy]
version = "v1"

[[architecture.policy.layers]]
id = "api"
paths = ["crates/api/**"]
"#,
        );
        std::fs::write(&path, raw).unwrap();

        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        assert_eq!(
            loaded.architecture.rules,
            std::path::Path::new(".ok/architecture-rules.yml")
        );

        let policy = load_architecture_policy(dir.path()).unwrap().unwrap();
        assert_eq!(policy.source, PolicySource::Compatibility);
        assert_eq!(policy.layers.len(), 1);
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
    fn memory_and_runtime_default_to_unconfigured_and_survive_a_round_trip() {
        // Both gates decide what the MCP surface advertises, so a default that
        // drifted to `true` would silently re-add five tools.
        let config = OkConfig::default();
        assert!(!config.memory.enabled);
        assert!(!config.runtime.enabled);
        assert!(config.runtime.organization.is_none());
        assert!(config.runtime.project.is_none());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.toml");
        OkConfig::write_default(&path).unwrap();
        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        assert!(!loaded.memory.enabled);
        assert!(!loaded.runtime.enabled);
        assert_eq!(loaded.runtime.provider, "sentry");
        assert_eq!(loaded.runtime.auth_token_env, "SENTRY_AUTH_TOKEN");
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
    fn default_toml_prints_declared_decimals_and_a_quiet_runtime_section() {
        let raw = OkConfig::default_toml().unwrap();
        assert!(raw.contains("graph_proximity = 0.35\n"), "{raw}");
        assert!(raw.contains("memory_signal = 0.2\n"), "{raw}");
        assert!(raw.contains("runtime_corroboration = 0.3\n"), "{raw}");
        assert!(!raw.contains("0.3499999940395355"), "{raw}");
        assert!(
            raw.contains(
                "# How call, inheritance and type-use edges are resolved while indexing:\n"
            ),
            "{raw}"
        );
        assert!(raw.contains("resolution_mode = \"shadow\"\n"), "{raw}");
        let runtime = raw
            .split("[runtime]\n")
            .nth(1)
            .and_then(|rest| rest.split("\n[").next())
            .expect("a [runtime] section");
        assert!(runtime.contains("enabled = false\n"), "{runtime}");
        assert!(!runtime.contains("\nprovider = "), "{runtime}");
        assert!(!runtime.contains("\nauth_token_env = "), "{runtime}");
        assert!(runtime.contains("# provider = \"sentry\""), "{runtime}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.toml"), &raw).unwrap();
        let loaded = OkConfig::load_from_repo(dir.path()).unwrap();
        let defaults = OkConfig::default();
        assert_eq!(
            loaded.ranking.graph_proximity,
            defaults.ranking.graph_proximity
        );
        assert_eq!(loaded.ranking.memory_signal, defaults.ranking.memory_signal);
        assert_eq!(
            loaded.ranking.semantic_similarity,
            defaults.ranking.semantic_similarity
        );
        assert_eq!(loaded.index.resolution_mode, ResolutionMode::Shadow);
        assert!(!loaded.runtime.enabled);
        assert_eq!(loaded.runtime.provider, defaults.runtime.provider);
        assert_eq!(
            loaded.runtime.auth_token_env,
            defaults.runtime.auth_token_env
        );
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
        assert_eq!(loaded.index.resolution_mode, ResolutionMode::Shadow);
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
