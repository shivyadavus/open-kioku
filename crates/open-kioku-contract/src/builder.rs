use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractBuildInput {
    pub task: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractBuildMetadata {
    pub built_at: DateTime<Utc>,
    pub builder_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractBoundary {
    pub allowed_files: Vec<String>,
    pub caution_files: Vec<String>,
    pub forbidden_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractRiskSummary {
    pub score: f64,
    pub level: String,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractConfidenceSummary {
    pub score: f64,
    pub level: String,
    pub gaps: Vec<String>,
}
