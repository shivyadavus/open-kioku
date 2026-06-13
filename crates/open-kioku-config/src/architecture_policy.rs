use globset::Glob;
use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const CANONICAL_ARCHITECTURE_POLICY_PATH: &str = ".open-kioku/architecture.toml";
pub const COMPATIBILITY_ARCHITECTURE_POLICY_PATH: &str = "ok.toml";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    #[default]
    Unspecified,
    Canonical,
    Compatibility,
    MatchingSources,
    Explicit,
}

impl fmt::Display for PolicySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unspecified => "unspecified",
            Self::Canonical => "canonical",
            Self::Compatibility => "compatibility",
            Self::MatchingSources => "matching_sources",
            Self::Explicit => "explicit",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyAction {
    Allow,
    Forbid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyLayer {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyContext {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRule {
    pub id: String,
    pub from: String,
    pub to: String,
    pub action: DependencyAction,
    pub severity: Severity,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicApiRule {
    pub id: String,
    pub component: String,
    pub public_globs: Vec<String>,
    #[serde(default)]
    pub internal_globs: Vec<String>,
    pub severity: Severity,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExemptionRule {
    pub id: String,
    pub rules: Vec<String>,
    pub paths: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitecturePolicy {
    pub version: PolicyVersion,
    #[serde(default)]
    pub layers: Vec<PolicyLayer>,
    #[serde(default)]
    pub contexts: Vec<PolicyContext>,
    #[serde(default)]
    pub dependency_rules: Vec<DependencyRule>,
    #[serde(default)]
    pub public_api_rules: Vec<PublicApiRule>,
    #[serde(default)]
    pub exemptions: Vec<ExemptionRule>,
    #[serde(skip, default)]
    pub source: PolicySource,
}

impl ArchitecturePolicy {
    pub fn validate(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if self.layers.is_empty() && self.contexts.is_empty() {
            return Err(policy_error(
                path,
                "policy",
                "layers/contexts",
                "must define at least one layer or bounded context",
            ));
        }

        let mut components = BTreeSet::new();
        for (kind, entries) in [
            (
                "layers",
                self.layers
                    .iter()
                    .map(|layer| (&layer.id, &layer.description, &layer.paths))
                    .collect::<Vec<_>>(),
            ),
            (
                "contexts",
                self.contexts
                    .iter()
                    .map(|context| (&context.id, &context.description, &context.paths))
                    .collect::<Vec<_>>(),
            ),
        ] {
            for (index, (id, description, paths)) in entries.into_iter().enumerate() {
                validate_id(path, &format!("{kind}[{index}]"), "id", id)?;
                if !components.insert(id.as_str()) {
                    return Err(policy_error(
                        path,
                        &format!("{kind}[{index}]"),
                        "id",
                        format!("duplicate component id `{id}`"),
                    ));
                }
                if let Some(description) = description {
                    validate_text(
                        path,
                        &format!("{kind}[{index}]"),
                        "description",
                        description,
                    )?;
                }
                validate_globs(path, &format!("{kind}[{index}]"), "paths", paths, true)?;
            }
        }

        let mut rule_ids = BTreeSet::new();
        for (index, rule) in self.dependency_rules.iter().enumerate() {
            let item = format!("dependency_rules[{index}]");
            validate_rule_id(path, &item, &rule.id, &mut rule_ids)?;
            validate_component_ref(path, &item, "from", &rule.from, &components)?;
            validate_component_ref(path, &item, "to", &rule.to, &components)?;
            validate_text(path, &item, "reason", &rule.reason)?;
        }
        for (index, rule) in self.public_api_rules.iter().enumerate() {
            let item = format!("public_api_rules[{index}]");
            validate_rule_id(path, &item, &rule.id, &mut rule_ids)?;
            validate_component_ref(path, &item, "component", &rule.component, &components)?;
            validate_globs(path, &item, "public_globs", &rule.public_globs, true)?;
            validate_globs(path, &item, "internal_globs", &rule.internal_globs, false)?;
            validate_text(path, &item, "reason", &rule.reason)?;
        }

        let mut exemption_ids = BTreeSet::new();
        for (index, exemption) in self.exemptions.iter().enumerate() {
            let item = format!("exemptions[{index}]");
            validate_id(path, &item, "id", &exemption.id)?;
            if !exemption_ids.insert(exemption.id.as_str()) {
                return Err(policy_error(
                    path,
                    &item,
                    "id",
                    format!("duplicate exemption id `{}`", exemption.id),
                ));
            }
            if exemption.rules.is_empty() {
                return Err(policy_error(
                    path,
                    &item,
                    "rules",
                    "must reference at least one policy rule",
                ));
            }
            for rule in &exemption.rules {
                if !rule_ids.contains(rule.as_str()) {
                    return Err(policy_error(
                        path,
                        &item,
                        "rules",
                        format!("references unknown rule `{rule}`"),
                    ));
                }
            }
            validate_globs(path, &item, "paths", &exemption.paths, true)?;
            validate_text(path, &item, "reason", &exemption.reason)?;
        }
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|error| {
            OkError::Config(format!("cannot serialize architecture policy: {error}"))
        })
    }

    pub fn source_paths(&self, repo: impl AsRef<Path>) -> Vec<PathBuf> {
        let repo = repo.as_ref();
        match self.source {
            PolicySource::Canonical => vec![repo.join(CANONICAL_ARCHITECTURE_POLICY_PATH)],
            PolicySource::Compatibility => {
                vec![repo.join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH)]
            }
            PolicySource::MatchingSources => vec![
                repo.join(CANONICAL_ARCHITECTURE_POLICY_PATH),
                repo.join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH),
            ],
            PolicySource::Unspecified | PolicySource::Explicit => Vec::new(),
        }
    }

    fn same_definition(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.source = PolicySource::Unspecified;
        right.source = PolicySource::Unspecified;
        left == right
    }
}

#[derive(Debug, Deserialize)]
struct CompatibilityConfig {
    #[serde(default)]
    architecture: Option<CompatibilityArchitectureConfig>,
}

#[derive(Debug, Deserialize)]
struct CompatibilityArchitectureConfig {
    #[serde(default)]
    policy: Option<ArchitecturePolicy>,
}

pub fn load_architecture_policy(repo_path: impl AsRef<Path>) -> Result<Option<ArchitecturePolicy>> {
    let repo_path = repo_path.as_ref();
    let canonical_path = repo_path.join(CANONICAL_ARCHITECTURE_POLICY_PATH);
    let compatibility_path = repo_path.join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH);

    let canonical = if canonical_path.exists() {
        Some(load_policy_file(&canonical_path, PolicySource::Canonical)?)
    } else {
        None
    };
    let compatibility = if compatibility_path.exists() {
        load_compatibility_policy(&compatibility_path)?
    } else {
        None
    };

    match (canonical, compatibility) {
        (None, None) => Ok(None),
        (Some(policy), None) | (None, Some(policy)) => Ok(Some(policy)),
        (Some(mut canonical), Some(compatibility)) => {
            if !canonical.same_definition(&compatibility) {
                return Err(OkError::Config(format!(
                    "conflicting architecture policies: canonical `{}` and compatibility alias `{}` define different policies",
                    canonical_path.display(),
                    compatibility_path.display()
                )));
            }
            canonical.source = PolicySource::MatchingSources;
            Ok(Some(canonical))
        }
    }
}

pub fn load_architecture_policy_from_path(path: impl AsRef<Path>) -> Result<ArchitecturePolicy> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(OkError::Config(format!(
            "architecture policy file does not exist: {}",
            path.display()
        )));
    }
    load_policy_file(path, PolicySource::Explicit)
}

fn load_policy_file(path: &Path, source: PolicySource) -> Result<ArchitecturePolicy> {
    let raw = fs::read_to_string(path)?;
    let mut policy: ArchitecturePolicy = toml::from_str(&raw).map_err(|error| {
        OkError::Config(format!(
            "invalid architecture policy at `{}`: {error}",
            path.display()
        ))
    })?;
    policy.source = source;
    policy.validate(path)?;
    Ok(policy)
}

fn load_compatibility_policy(path: &Path) -> Result<Option<ArchitecturePolicy>> {
    let raw = fs::read_to_string(path)?;
    let parsed: CompatibilityConfig = toml::from_str(&raw).map_err(|error| {
        OkError::Config(format!(
            "invalid architecture policy compatibility source at `{}`: {error}",
            path.display()
        ))
    })?;
    let Some(mut policy) = parsed
        .architecture
        .and_then(|architecture| architecture.policy)
    else {
        return Ok(None);
    };
    policy.source = PolicySource::Compatibility;
    policy.validate(path)?;
    Ok(Some(policy))
}

fn validate_rule_id<'a>(
    path: &Path,
    item: &str,
    id: &'a str,
    rule_ids: &mut BTreeSet<&'a str>,
) -> Result<()> {
    validate_id(path, item, "id", id)?;
    if !rule_ids.insert(id) {
        return Err(policy_error(
            path,
            item,
            "id",
            format!("duplicate rule id `{id}`"),
        ));
    }
    Ok(())
}

fn validate_component_ref(
    path: &Path,
    item: &str,
    field: &str,
    value: &str,
    components: &BTreeSet<&str>,
) -> Result<()> {
    validate_text(path, item, field, value)?;
    if !components.contains(value) {
        return Err(policy_error(
            path,
            item,
            field,
            format!("references unknown component `{value}`"),
        ));
    }
    Ok(())
}

fn validate_id(path: &Path, item: &str, field: &str, value: &str) -> Result<()> {
    validate_text(path, item, field, value)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(policy_error(
            path,
            item,
            field,
            "must contain only ASCII letters, digits, `.`, `_`, or `-`",
        ));
    }
    Ok(())
}

fn validate_text(path: &Path, item: &str, field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(policy_error(path, item, field, "must not be empty"));
    }
    Ok(())
}

fn validate_globs(
    path: &Path,
    item: &str,
    field: &str,
    patterns: &[String],
    required: bool,
) -> Result<()> {
    if required && patterns.is_empty() {
        return Err(policy_error(
            path,
            item,
            field,
            "must contain at least one glob",
        ));
    }
    let mut unique_patterns = BTreeSet::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let indexed_field = format!("{field}[{index}]");
        validate_text(path, item, &indexed_field, pattern)?;
        if !unique_patterns.insert(pattern.as_str()) {
            return Err(policy_error(
                path,
                item,
                &indexed_field,
                format!("duplicates glob `{pattern}`"),
            ));
        }
        let bytes = pattern.as_bytes();
        let has_windows_drive_prefix =
            bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        if pattern.starts_with('/')
            || pattern.starts_with('\\')
            || has_windows_drive_prefix
            || pattern.contains('\\')
            || pattern.split('/').any(|component| component == "..")
        {
            return Err(policy_error(
                path,
                item,
                &indexed_field,
                format!("glob `{pattern}` must stay inside the repository and use `/` separators"),
            ));
        }
        Glob::new(pattern).map_err(|error| {
            policy_error(
                path,
                item,
                &indexed_field,
                format!("invalid glob `{pattern}`: {error}"),
            )
        })?;
    }
    Ok(())
}

fn policy_error(path: &Path, item: &str, field: &str, message: impl fmt::Display) -> OkError {
    OkError::Config(format!(
        "invalid architecture policy at `{}`: {item} field `{field}` {message}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../examples/architecture-policy.toml");

    fn write_canonical(repo: &Path, content: &str) -> PathBuf {
        let path = repo.join(CANONICAL_ARCHITECTURE_POLICY_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    fn write_alias(repo: &Path, content: &str) {
        fs::write(
            repo.join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH),
            format!("[architecture]\nrules = \".ok/architecture-rules.yml\"\n\n{content}"),
        )
        .unwrap();
    }

    fn alias_content(policy: &str) -> String {
        policy
            .lines()
            .map(|line| {
                if line.starts_with("[[") {
                    line.replacen("[[", "[[architecture.policy.", 1)
                } else if line.starts_with('[') {
                    line.replacen('[', "[architecture.policy.", 1)
                } else if line.starts_with("version =") {
                    format!("[architecture.policy]\n{line}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn example_round_trips_cleanly() {
        let policy: ArchitecturePolicy = toml::from_str(EXAMPLE).unwrap();
        policy.validate("example.toml").unwrap();
        let serialized = policy.to_toml().unwrap();
        let reparsed: ArchitecturePolicy = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed, policy);
    }

    #[test]
    fn loads_canonical_policy_deterministically() {
        let repo = tempfile::tempdir().unwrap();
        write_canonical(repo.path(), EXAMPLE);
        let policy = load_architecture_policy(repo.path()).unwrap().unwrap();
        assert_eq!(policy.source, PolicySource::Canonical);
        assert_eq!(policy.layers.len(), 3);
    }

    #[test]
    fn loads_ok_toml_compatibility_alias() {
        let repo = tempfile::tempdir().unwrap();
        write_alias(repo.path(), &alias_content(EXAMPLE));
        let policy = load_architecture_policy(repo.path()).unwrap().unwrap();
        assert_eq!(policy.source, PolicySource::Compatibility);
        assert_eq!(policy.dependency_rules.len(), 2);
    }

    #[test]
    fn matching_sources_resolve_to_canonical() {
        let repo = tempfile::tempdir().unwrap();
        write_canonical(repo.path(), EXAMPLE);
        write_alias(repo.path(), &alias_content(EXAMPLE));
        let policy = load_architecture_policy(repo.path()).unwrap().unwrap();
        assert_eq!(policy.source, PolicySource::MatchingSources);
        assert_eq!(
            policy.source_paths(repo.path()),
            vec![
                repo.path().join(CANONICAL_ARCHITECTURE_POLICY_PATH),
                repo.path().join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH),
            ]
        );
    }

    #[test]
    fn conflicting_sources_fail_loudly() {
        let repo = tempfile::tempdir().unwrap();
        write_canonical(repo.path(), EXAMPLE);
        write_alias(
            repo.path(),
            &alias_content(&EXAMPLE.replace("api-must-not-depend-on-storage", "different-rule")),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "configuration error: conflicting architecture policies: canonical `{}` and compatibility alias `{}` define different policies",
                repo.path().join(CANONICAL_ARCHITECTURE_POLICY_PATH).display(),
                repo.path()
                    .join(COMPATIBILITY_ARCHITECTURE_POLICY_PATH)
                    .display()
            )
        );
    }

    #[test]
    fn rejects_unknown_severity_with_source_path() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(
            repo.path(),
            &EXAMPLE.replace("severity = \"error\"", "severity = \"urgent\""),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("unknown variant `urgent`"));
    }

    #[test]
    fn rejects_malformed_glob_with_rule_and_field() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(repo.path(), &EXAMPLE.replace("crates/*-api/**", "["));
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("layers[0] field `paths[0]` invalid glob `[`"));
    }

    #[test]
    fn rejects_globs_that_escape_the_repository() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(
            repo.path(),
            &EXAMPLE.replace("crates/*-api/**", "../outside/**"),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("must stay inside the repository"));
    }

    #[test]
    fn rejects_duplicate_globs_with_field_location() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(
            repo.path(),
            &EXAMPLE.replace(
                "paths = [\"crates/*-api/**\"]",
                "paths = [\"crates/*-api/**\", \"crates/*-api/**\"]",
            ),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "configuration error: invalid architecture policy at `{}`: layers[0] field `paths[1]` duplicates glob `crates/*-api/**`",
                path.display()
            )
        );
    }

    #[test]
    fn rejects_empty_component_descriptions() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(
            repo.path(),
            &EXAMPLE.replace(
                "description = \"Public request and response boundary\"",
                "description = \"  \"",
            ),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "configuration error: invalid architecture policy at `{}`: layers[0] field `description` must not be empty",
                path.display()
            )
        );
    }

    #[test]
    fn rejects_unknown_policy_fields() {
        let repo = tempfile::tempdir().unwrap();
        let path = write_canonical(
            repo.path(),
            &EXAMPLE.replace(
                "description = \"Public request and response boundary\"",
                "description = \"Public request and response boundary\"\npathz = [\"src/**\"]",
            ),
        );
        let error = load_architecture_policy(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("unknown field `pathz`"));
    }

    #[test]
    fn repositories_without_policy_return_none() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(
            repo.path().join("ok.toml"),
            "[architecture]\nrules = \"rules.yml\"\n",
        )
        .unwrap();
        assert!(load_architecture_policy(repo.path()).unwrap().is_none());
    }
}
