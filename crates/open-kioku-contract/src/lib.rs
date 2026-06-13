use chrono::{DateTime, Utc};
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use thiserror::Error;

/// The schema version written into every persisted or exchanged contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractVersion {
    V1,
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("v1")
    }
}

/// Stable identifier for a change contract.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ContractId(#[schemars(length(min = 1))] pub String);

impl ContractId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for ContractId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable reference to evidence already produced by Open Kioku.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EvidenceRef(#[schemars(length(min = 1))] pub String);

impl EvidenceRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// Repository-relative file path used by contract boundaries.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ContractFile(#[schemars(length(min = 1))] pub String);

impl ContractFile {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self(path.as_ref().to_string_lossy().replace('\\', "/"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

/// Stable symbol identifier or qualified symbol name.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ImpactedSymbol(#[schemars(length(min = 1))] pub String);

impl ImpactedSymbol {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequiredTest {
    #[schemars(length(min = 1))]
    pub target: String,
    #[schemars(length(min = 1))]
    pub reason: String,
    #[schemars(length(min = 1))]
    pub evidence_refs: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSeverity {
    Advisory,
    Required,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchitectureConstraint {
    #[schemars(length(min = 1))]
    pub rule: String,
    pub severity: ConstraintSeverity,
    #[schemars(length(min = 1))]
    pub reason: String,
    #[schemars(length(min = 1))]
    pub evidence_refs: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidationCommand {
    #[schemars(length(min = 1))]
    pub command: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn from_score(score: f64) -> Self {
        if score >= 0.85 {
            Self::Critical
        } else if score >= 0.65 {
            Self::High
        } else if score >= 0.35 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub score: f64,
    #[schemars(length(min = 1))]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    Low,
    Medium,
    High,
    Exact,
}

impl ConfidenceLevel {
    pub fn from_score(score: f64) -> Self {
        if score >= 0.95 {
            Self::Exact
        } else if score >= 0.75 {
            Self::High
        } else if score >= 0.55 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfidenceAssessment {
    pub level: ConfidenceLevel,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub score: f64,
    #[schemars(length(min = 1))]
    pub basis: Vec<String>,
    /// Known gaps must be represented explicitly rather than hidden in prose.
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractTimestamps {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourcePlanRef {
    #[schemars(length(min = 1))]
    pub id: String,
    #[schemars(length(min = 1))]
    pub digest: String,
}

/// Canonical v1 artifact connecting a task to evidence, boundaries, and validation.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ChangeContractV1 {
    pub id: ContractId,
    pub version: ContractVersion,
    #[schemars(length(min = 1))]
    pub task: String,
    #[schemars(length(min = 1))]
    pub evidence_refs: Vec<EvidenceRef>,
    #[schemars(length(min = 1))]
    pub primary_files: Vec<ContractFile>,
    pub secondary_files: Vec<ContractFile>,
    pub forbidden_files: Vec<ContractFile>,
    #[schemars(length(min = 1))]
    pub impacted_symbols: Vec<ImpactedSymbol>,
    #[schemars(length(min = 1))]
    pub required_tests: Vec<RequiredTest>,
    #[schemars(length(min = 1))]
    pub architecture_constraints: Vec<ArchitectureConstraint>,
    #[schemars(length(min = 1))]
    pub validation_commands: Vec<ValidationCommand>,
    pub risk: RiskAssessment,
    pub confidence: ConfidenceAssessment,
    pub timestamps: ContractTimestamps,
    pub source_plan_ref: SourcePlanRef,
    /// Additive producer metadata is preserved during v1 round trips.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct UnvalidatedChangeContractV1 {
    id: ContractId,
    version: ContractVersion,
    task: String,
    evidence_refs: Vec<EvidenceRef>,
    primary_files: Vec<ContractFile>,
    secondary_files: Vec<ContractFile>,
    forbidden_files: Vec<ContractFile>,
    impacted_symbols: Vec<ImpactedSymbol>,
    required_tests: Vec<RequiredTest>,
    architecture_constraints: Vec<ArchitectureConstraint>,
    validation_commands: Vec<ValidationCommand>,
    risk: RiskAssessment,
    confidence: ConfidenceAssessment,
    timestamps: ContractTimestamps,
    source_plan_ref: SourcePlanRef,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

impl From<UnvalidatedChangeContractV1> for ChangeContractV1 {
    fn from(value: UnvalidatedChangeContractV1) -> Self {
        Self {
            id: value.id,
            version: value.version,
            task: value.task,
            evidence_refs: value.evidence_refs,
            primary_files: value.primary_files,
            secondary_files: value.secondary_files,
            forbidden_files: value.forbidden_files,
            impacted_symbols: value.impacted_symbols,
            required_tests: value.required_tests,
            architecture_constraints: value.architecture_constraints,
            validation_commands: value.validation_commands,
            risk: value.risk,
            confidence: value.confidence,
            timestamps: value.timestamps,
            source_plan_ref: value.source_plan_ref,
            extensions: value.extensions,
        }
    }
}

impl<'de> Deserialize<'de> for ChangeContractV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let contract = Self::from(UnvalidatedChangeContractV1::deserialize(deserializer)?);
        contract.validate().map_err(de::Error::custom)?;
        Ok(contract)
    }
}

impl ChangeContractV1 {
    pub fn validate(&self) -> Result<(), ContractValidationErrors> {
        let mut violations = Vec::new();

        require_text(&mut violations, "id", &self.id.0);
        require_text(&mut violations, "task", &self.task);
        require_non_empty(&mut violations, "evidence_refs", &self.evidence_refs);
        require_non_empty(&mut violations, "primary_files", &self.primary_files);
        require_non_empty(&mut violations, "impacted_symbols", &self.impacted_symbols);
        require_non_empty(&mut violations, "required_tests", &self.required_tests);
        require_non_empty(
            &mut violations,
            "architecture_constraints",
            &self.architecture_constraints,
        );
        require_non_empty(
            &mut violations,
            "validation_commands",
            &self.validation_commands,
        );

        validate_unique(&mut violations, "evidence_refs", &self.evidence_refs);
        validate_unique(&mut violations, "primary_files", &self.primary_files);
        validate_unique(&mut violations, "secondary_files", &self.secondary_files);
        validate_unique(&mut violations, "forbidden_files", &self.forbidden_files);
        validate_unique(&mut violations, "impacted_symbols", &self.impacted_symbols);

        for (field, files) in [
            ("primary_files", &self.primary_files),
            ("secondary_files", &self.secondary_files),
            ("forbidden_files", &self.forbidden_files),
        ] {
            for (index, file) in files.iter().enumerate() {
                validate_repo_path(&mut violations, &format!("{field}[{index}]"), file.as_str());
            }
        }

        validate_disjoint_files(
            &mut violations,
            "primary_files",
            &self.primary_files,
            "secondary_files",
            &self.secondary_files,
        );
        validate_disjoint_files(
            &mut violations,
            "primary_files",
            &self.primary_files,
            "forbidden_files",
            &self.forbidden_files,
        );
        validate_disjoint_files(
            &mut violations,
            "secondary_files",
            &self.secondary_files,
            "forbidden_files",
            &self.forbidden_files,
        );

        for (index, evidence) in self.evidence_refs.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("evidence_refs[{index}]"),
                &evidence.0,
            );
        }
        for (index, symbol) in self.impacted_symbols.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("impacted_symbols[{index}]"),
                &symbol.0,
            );
        }
        for (index, test) in self.required_tests.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("required_tests[{index}].target"),
                &test.target,
            );
            require_text(
                &mut violations,
                &format!("required_tests[{index}].reason"),
                &test.reason,
            );
            require_non_empty(
                &mut violations,
                &format!("required_tests[{index}].evidence_refs"),
                &test.evidence_refs,
            );
            validate_evidence_refs(
                &mut violations,
                &format!("required_tests[{index}].evidence_refs"),
                &test.evidence_refs,
            );
        }
        for (index, constraint) in self.architecture_constraints.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("architecture_constraints[{index}].rule"),
                &constraint.rule,
            );
            require_text(
                &mut violations,
                &format!("architecture_constraints[{index}].reason"),
                &constraint.reason,
            );
            require_non_empty(
                &mut violations,
                &format!("architecture_constraints[{index}].evidence_refs"),
                &constraint.evidence_refs,
            );
            validate_evidence_refs(
                &mut violations,
                &format!("architecture_constraints[{index}].evidence_refs"),
                &constraint.evidence_refs,
            );
        }
        for (index, command) in self.validation_commands.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("validation_commands[{index}].command"),
                &command.command,
            );
            require_text(
                &mut violations,
                &format!("validation_commands[{index}].reason"),
                &command.reason,
            );
        }

        validate_score(&mut violations, "risk.score", self.risk.score);
        require_non_empty(&mut violations, "risk.reasons", &self.risk.reasons);
        for (index, reason) in self.risk.reasons.iter().enumerate() {
            require_text(&mut violations, &format!("risk.reasons[{index}]"), reason);
        }
        if self.risk.level != RiskLevel::from_score(self.risk.score) {
            violations.push(ContractViolation::new(
                "risk.level",
                "must match the level derived from risk.score",
            ));
        }

        validate_score(&mut violations, "confidence.score", self.confidence.score);
        require_non_empty(&mut violations, "confidence.basis", &self.confidence.basis);
        for (index, basis) in self.confidence.basis.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("confidence.basis[{index}]"),
                basis,
            );
        }
        for (index, uncertainty) in self.confidence.uncertainty.iter().enumerate() {
            require_text(
                &mut violations,
                &format!("confidence.uncertainty[{index}]"),
                uncertainty,
            );
        }
        if self.confidence.level != ConfidenceLevel::from_score(self.confidence.score) {
            violations.push(ContractViolation::new(
                "confidence.level",
                "must match the level derived from confidence.score",
            ));
        }
        if self.confidence.level != ConfidenceLevel::Exact && self.confidence.uncertainty.is_empty()
        {
            violations.push(ContractViolation::new(
                "confidence.uncertainty",
                "must describe known uncertainty unless confidence is exact",
            ));
        }

        if self.timestamps.updated_at < self.timestamps.created_at {
            violations.push(ContractViolation::new(
                "timestamps.updated_at",
                "must be at or after timestamps.created_at",
            ));
        }
        require_text(
            &mut violations,
            "source_plan_ref.id",
            &self.source_plan_ref.id,
        );
        require_text(
            &mut violations,
            "source_plan_ref.digest",
            &self.source_plan_ref.digest,
        );
        for key in self.extensions.keys() {
            if CONTRACT_FIELDS.contains(&key.as_str()) {
                violations.push(ContractViolation::new(
                    format!("extensions.{key}"),
                    "must not shadow a contract field",
                ));
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(ContractValidationErrors { violations })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractViolation {
    pub field: String,
    pub message: String,
}

impl ContractViolation {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("change contract validation failed: {violations}", violations = format_violations(.violations))]
pub struct ContractValidationErrors {
    pub violations: Vec<ContractViolation>,
}

fn format_violations(violations: &[ContractViolation]) -> String {
    violations
        .iter()
        .map(|violation| format!("{} {}", violation.field, violation.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn require_text(violations: &mut Vec<ContractViolation>, field: &str, value: &str) {
    if value.trim().is_empty() {
        violations.push(ContractViolation::new(field, "must not be empty"));
    }
}

fn require_non_empty<T>(violations: &mut Vec<ContractViolation>, field: &str, values: &[T]) {
    if values.is_empty() {
        violations.push(ContractViolation::new(field, "must not be empty"));
    }
}

fn validate_unique<T>(violations: &mut Vec<ContractViolation>, field: &str, values: &[T])
where
    T: Ord,
{
    let mut seen = BTreeSet::new();
    if values.iter().any(|value| !seen.insert(value)) {
        violations.push(ContractViolation::new(
            field,
            "must not contain duplicate entries",
        ));
    }
}

fn validate_repo_path(violations: &mut Vec<ContractViolation>, field: &str, path: &str) {
    if path.trim().is_empty() {
        violations.push(ContractViolation::new(field, "must not be empty"));
        return;
    }
    if path != path.trim() {
        violations.push(ContractViolation::new(
            field,
            "must not have leading or trailing whitespace",
        ));
    }
    let bytes = path.as_bytes();
    let has_windows_drive_prefix =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if path.starts_with('/') || path.starts_with('\\') || has_windows_drive_prefix {
        violations.push(ContractViolation::new(field, "must be repository-relative"));
    }
    if path.contains('\\') {
        violations.push(ContractViolation::new(field, "must use forward slashes"));
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        violations.push(ContractViolation::new(
            field,
            "must be a normalized path inside the repository",
        ));
    }
}

fn validate_disjoint_files(
    violations: &mut Vec<ContractViolation>,
    left_name: &str,
    left: &[ContractFile],
    right_name: &str,
    right: &[ContractFile],
) {
    let left = left.iter().collect::<BTreeSet<_>>();
    if left.intersection(&right.iter().collect()).next().is_some() {
        violations.push(ContractViolation::new(
            format!("{left_name}/{right_name}"),
            "must not overlap",
        ));
    }
}

fn validate_evidence_refs(
    violations: &mut Vec<ContractViolation>,
    field: &str,
    evidence_refs: &[EvidenceRef],
) {
    validate_unique(violations, field, evidence_refs);
    for (index, evidence_ref) in evidence_refs.iter().enumerate() {
        require_text(violations, &format!("{field}[{index}]"), &evidence_ref.0);
    }
}

fn validate_score(violations: &mut Vec<ContractViolation>, field: &str, score: f64) {
    if !score.is_finite() || !(0.0..=1.0).contains(&score) {
        violations.push(ContractViolation::new(
            field,
            "must be a finite number between 0.0 and 1.0",
        ));
    }
}

const CONTRACT_FIELDS: &[&str] = &[
    "id",
    "version",
    "task",
    "evidence_refs",
    "primary_files",
    "secondary_files",
    "forbidden_files",
    "impacted_symbols",
    "required_tests",
    "architecture_constraints",
    "validation_commands",
    "risk",
    "confidence",
    "timestamps",
    "source_plan_ref",
];

/// Return the authoritative JSON Schema root for `ChangeContractV1`.
pub fn schema() -> RootSchema {
    schema_for!(ChangeContractV1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> ChangeContractV1 {
        serde_json::from_str(include_str!("../tests/fixtures/change_contract_v1.json")).unwrap()
    }

    #[test]
    fn golden_fixture_round_trips_losslessly() {
        let original: Value =
            serde_json::from_str(include_str!("../tests/fixtures/change_contract_v1.json"))
                .unwrap();
        let contract: ChangeContractV1 = serde_json::from_value(original.clone()).unwrap();
        let serialized = serde_json::to_value(contract).unwrap();
        assert_eq!(serialized, original);
    }

    #[test]
    fn schema_requires_every_contract_field() {
        let schema = schema();
        let object = schema.schema.object.as_ref().unwrap();
        let expected = [
            "id",
            "version",
            "task",
            "evidence_refs",
            "primary_files",
            "secondary_files",
            "forbidden_files",
            "impacted_symbols",
            "required_tests",
            "architecture_constraints",
            "validation_commands",
            "risk",
            "confidence",
            "timestamps",
            "source_plan_ref",
        ];
        for field in expected {
            assert!(object.required.contains(field), "{field} is not required");
        }
    }

    #[test]
    fn generated_schema_describes_the_golden_fixture_shape() {
        let generated = serde_json::to_value(schema()).unwrap();
        assert_eq!(generated["type"], json!("object"));
        assert_eq!(generated["properties"]["task"]["minLength"], json!(1));
        assert_eq!(
            generated["properties"]["primary_files"]["minItems"],
            json!(1)
        );
        assert_eq!(
            generated["definitions"]["RiskAssessment"]["properties"]["score"]["maximum"],
            json!(1.0)
        );
        assert_eq!(
            generated["definitions"]["ConfidenceAssessment"]["properties"]["score"]["minimum"],
            json!(0.0)
        );
        assert_eq!(
            generated["definitions"]["ContractVersion"]["enum"],
            json!(["v1"])
        );
    }

    #[test]
    fn rejects_every_missing_required_field() {
        for field in [
            "id",
            "version",
            "task",
            "evidence_refs",
            "primary_files",
            "secondary_files",
            "forbidden_files",
            "impacted_symbols",
            "required_tests",
            "architecture_constraints",
            "validation_commands",
            "risk",
            "confidence",
            "timestamps",
            "source_plan_ref",
        ] {
            let mut value = serde_json::to_value(fixture()).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let error = serde_json::from_value::<ChangeContractV1>(value)
                .unwrap_err()
                .to_string();
            assert!(error.contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn rejects_unsupported_versions() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["version"] = json!("v2");
        assert!(serde_json::from_value::<ChangeContractV1>(value).is_err());
    }

    #[test]
    fn rejects_invalid_timestamp_syntax_and_order() {
        let mut invalid_syntax = serde_json::to_value(fixture()).unwrap();
        invalid_syntax["timestamps"]["created_at"] = json!("not-a-timestamp");
        assert!(serde_json::from_value::<ChangeContractV1>(invalid_syntax).is_err());

        let mut invalid_order = serde_json::to_value(fixture()).unwrap();
        invalid_order["timestamps"]["updated_at"] = json!("2026-06-12T09:59:59Z");
        let error = serde_json::from_value::<ChangeContractV1>(invalid_order)
            .unwrap_err()
            .to_string();
        assert!(error.contains("timestamps.updated_at"));
    }

    #[test]
    fn rejects_empty_mandatory_sections() {
        for field in [
            "evidence_refs",
            "primary_files",
            "impacted_symbols",
            "required_tests",
            "architecture_constraints",
            "validation_commands",
        ] {
            let mut value = serde_json::to_value(fixture()).unwrap();
            value[field] = json!([]);
            let error = serde_json::from_value::<ChangeContractV1>(value)
                .unwrap_err()
                .to_string();
            assert!(error.contains(field), "{field}: {error}");
        }

        type Mutation = fn(&mut Value);
        let nested_cases: [(&str, Mutation); 5] = [
            ("task", |value: &mut Value| value["task"] = json!(" ")),
            ("risk.reasons", |value: &mut Value| {
                value["risk"]["reasons"] = json!([])
            }),
            ("confidence.basis", |value: &mut Value| {
                value["confidence"]["basis"] = json!([])
            }),
            ("required_tests[0].evidence_refs", |value: &mut Value| {
                value["required_tests"][0]["evidence_refs"] = json!([])
            }),
            (
                "architecture_constraints[0].evidence_refs",
                |value: &mut Value| {
                    value["architecture_constraints"][0]["evidence_refs"] = json!([])
                },
            ),
        ];
        for (field, mutate) in nested_cases {
            let mut value = serde_json::to_value(fixture()).unwrap();
            mutate(&mut value);
            let error = serde_json::from_value::<ChangeContractV1>(value)
                .unwrap_err()
                .to_string();
            assert!(error.contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_scores_levels_and_hidden_uncertainty() {
        let mut invalid_score = serde_json::to_value(fixture()).unwrap();
        invalid_score["risk"]["score"] = json!(1.1);
        assert!(serde_json::from_value::<ChangeContractV1>(invalid_score).is_err());

        let mut mismatched_level = serde_json::to_value(fixture()).unwrap();
        mismatched_level["confidence"]["level"] = json!("exact");
        assert!(serde_json::from_value::<ChangeContractV1>(mismatched_level).is_err());

        let mut hidden_uncertainty = serde_json::to_value(fixture()).unwrap();
        hidden_uncertainty["confidence"]["uncertainty"] = json!([]);
        assert!(serde_json::from_value::<ChangeContractV1>(hidden_uncertainty).is_err());
    }

    #[test]
    fn rejects_unsafe_overlapping_and_duplicate_paths() {
        for path in [
            "/etc/passwd",
            "../outside.rs",
            r"C:\outside.rs",
            r"src\lib.rs",
            "src//lib.rs",
            "./src/lib.rs",
            "",
        ] {
            let mut value = serde_json::to_value(fixture()).unwrap();
            value["primary_files"] = json!([path]);
            assert!(serde_json::from_value::<ChangeContractV1>(value).is_err());
        }

        let mut overlap = serde_json::to_value(fixture()).unwrap();
        overlap["secondary_files"] = json!(["crates/open-kioku-contract/src/lib.rs"]);
        assert!(serde_json::from_value::<ChangeContractV1>(overlap).is_err());

        let mut duplicate = serde_json::to_value(fixture()).unwrap();
        duplicate["primary_files"] = json!([
            "crates/open-kioku-contract/src/lib.rs",
            "crates/open-kioku-contract/src/lib.rs"
        ]);
        assert!(serde_json::from_value::<ChangeContractV1>(duplicate).is_err());
    }

    #[test]
    fn preserves_additive_extension_fields() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["x-producer"] = json!({"name": "open-kioku-plan", "version": 1});
        let contract: ChangeContractV1 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(contract).unwrap(), value);
    }

    #[test]
    fn explicit_validation_catches_invalid_programmatic_values() {
        let mut contract = fixture();
        contract.task = " ".into();
        contract
            .primary_files
            .push(contract.primary_files[0].clone());
        contract.extensions.insert("task".into(), json!("shadow"));
        let error = contract.validate().unwrap_err();
        assert!(error.violations.iter().any(|item| item.field == "task"));
        assert!(error
            .violations
            .iter()
            .any(|item| item.field == "primary_files"));
        assert!(error
            .violations
            .iter()
            .any(|item| item.field == "extensions.task"));
    }
}
