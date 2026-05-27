use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SentryConfig {
    pub enabled: bool,
    pub organization: Option<String>,
    pub project: Option<String>,
    pub auth_token_env: String,
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            organization: None,
            project: None,
            auth_token_env: "SENTRY_AUTH_TOKEN".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeToolResponse {
    pub results: Vec<RuntimeSignal>,
    pub evidence: Vec<String>,
    pub confidence: String,
    pub reason: String,
    pub integration: String,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSignal {
    pub title: String,
    pub location_hint: Option<String>,
    pub source_url: Option<String>,
}

pub fn ensure_configured(config: &SentryConfig) -> Result<()> {
    if !config.enabled {
        return Err(OkError::Unsupported(
            "Sentry integration is disabled in configuration".into(),
        ));
    }
    if config
        .organization
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        return Err(OkError::Unsupported(
            "Sentry integration requires an organization".into(),
        ));
    }
    if config.project.as_deref().unwrap_or_default().is_empty() {
        return Err(OkError::Unsupported(
            "Sentry integration requires a project".into(),
        ));
    }
    if std::env::var(&config.auth_token_env)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(OkError::Unsupported(format!(
            "Sentry integration requires {} in the environment",
            config.auth_token_env
        )));
    }
    Ok(())
}

pub fn disabled_response(tool: &str) -> RuntimeToolResponse {
    RuntimeToolResponse {
        results: Vec::new(),
        evidence: vec!["runtime integrations are disabled by default".into()],
        confidence: "low".into(),
        reason: format!("{tool} requires an explicitly configured runtime provider"),
        integration: "sentry".into(),
        configured: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let err = ensure_configured(&SentryConfig::default()).unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn disabled_response_is_structured() {
        let response = disabled_response("find_recent_failures");
        assert!(!response.configured);
        assert_eq!(response.integration, "sentry");
        assert!(response.reason.contains("find_recent_failures"));
    }
}
