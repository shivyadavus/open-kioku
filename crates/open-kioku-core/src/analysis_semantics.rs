use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const ANALYSIS_SEMANTICS_DESCRIPTOR_VERSION: u32 = 1;
pub const STABLE_IDENTITY_SEMANTICS_VERSION: &str = "stable-identity-v1";
pub const PROJECT_RESOLVER_SEMANTICS_VERSION: &str = "project-resolver-v1";
pub const RELATIONSHIP_RESOLVER_SEMANTICS_VERSION: &str = "ri3-relationship-resolver-v1";
pub const PROOF_POLICY_SEMANTICS_VERSION: &str = "ri3-proof-policy-v1";
pub const GRAPH_EMISSION_SEMANTICS_VERSION: &str = "ri3-graph-emission-v1";
pub const EXACT_INDEX_INGESTION_SEMANTICS_VERSION: &str = "exact-occurrence-v1";
pub const LANGUAGE_ADAPTER_SEMANTICS_VERSION: &str = "ri3-language-semantics-v1";
pub const PARSER_SEMANTICS_VERSION: &str = "tier1-parser-semantics-v1";

const TIER1_LANGUAGES: [&str; 6] = ["go", "java", "javascript", "python", "rust", "typescript"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsDescriptor {
    pub version: u32,
    pub stable_identity_version: String,
    pub parser_semantics: BTreeMap<String, String>,
    pub project_resolver_version: String,
    pub relationship_resolver_version: String,
    pub proof_policy_version: String,
    pub graph_emission_version: String,
    pub exact_index_ingestion_version: String,
    pub language_adapter_versions: BTreeMap<String, String>,
}

impl AnalysisSemanticsDescriptor {
    pub fn fingerprint(&self) -> String {
        // Struct field order is fixed and all maps are BTreeMap, so serde_json emits a stable
        // canonical representation for this descriptor. These field types are infallibly JSON
        // serializable; failure indicates a programming invariant violation.
        let canonical = serde_json::to_vec(self)
            .expect("analysis semantics descriptor must be canonically serializable");
        format!("{:x}", Sha256::digest(canonical))
    }
}

pub fn current_analysis_semantics_descriptor() -> AnalysisSemanticsDescriptor {
    let parser_semantics = TIER1_LANGUAGES
        .into_iter()
        .map(|language| (language.to_string(), PARSER_SEMANTICS_VERSION.to_string()))
        .collect();
    let language_adapter_versions = TIER1_LANGUAGES
        .into_iter()
        .map(|language| {
            (
                language.to_string(),
                LANGUAGE_ADAPTER_SEMANTICS_VERSION.to_string(),
            )
        })
        .collect();
    AnalysisSemanticsDescriptor {
        version: ANALYSIS_SEMANTICS_DESCRIPTOR_VERSION,
        stable_identity_version: STABLE_IDENTITY_SEMANTICS_VERSION.into(),
        parser_semantics,
        project_resolver_version: PROJECT_RESOLVER_SEMANTICS_VERSION.into(),
        relationship_resolver_version: RELATIONSHIP_RESOLVER_SEMANTICS_VERSION.into(),
        proof_policy_version: PROOF_POLICY_SEMANTICS_VERSION.into(),
        graph_emission_version: GRAPH_EMISSION_SEMANTICS_VERSION.into(),
        exact_index_ingestion_version: EXACT_INDEX_INGESTION_SEMANTICS_VERSION.into(),
        language_adapter_versions,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsState {
    pub descriptor: AnalysisSemanticsDescriptor,
    pub fingerprint: String,
}

impl AnalysisSemanticsState {
    pub fn new(descriptor: AnalysisSemanticsDescriptor) -> Self {
        let fingerprint = descriptor.fingerprint();
        Self {
            descriptor,
            fingerprint,
        }
    }

    pub fn current() -> Self {
        Self::new(current_analysis_semantics_descriptor())
    }

    pub fn fingerprint_is_valid(&self) -> bool {
        self.fingerprint == self.descriptor.fingerprint()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisSemanticsCompatibilityStatus {
    Compatible,
    RefreshRequired,
    RebuildRequired,
    FutureUnsupported,
}

impl AnalysisSemanticsCompatibilityStatus {
    pub fn allows_authoritative_relationships(self) -> bool {
        matches!(self, Self::Compatible)
    }

    pub fn allows_partial_index_update(self) -> bool {
        matches!(self, Self::Compatible)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsCompatibility {
    pub status: AnalysisSemanticsCompatibilityStatus,
    pub stored_fingerprint: Option<String>,
    pub current_fingerprint: String,
    pub reasons: Vec<String>,
    pub affected_components: Vec<String>,
    pub affected_languages: Vec<String>,
    pub recommended_action: String,
}

impl AnalysisSemanticsCompatibility {
    pub fn compatible(current: &AnalysisSemanticsState) -> Self {
        Self {
            status: AnalysisSemanticsCompatibilityStatus::Compatible,
            stored_fingerprint: Some(current.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: Vec::new(),
            affected_components: Vec::new(),
            affected_languages: Vec::new(),
            recommended_action: "none".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisSemanticsCompatibilityPolicy {
    /// Components explicitly safe to refresh without rebuilding authoritative graph state.
    /// V1 intentionally leaves this empty: semantic changes fail closed until a component has
    /// a proven selective refresh path.
    pub refresh_compatible_components: BTreeSet<String>,
    pub legacy_manifest_requires_rebuild: bool,
}

impl Default for AnalysisSemanticsCompatibilityPolicy {
    fn default() -> Self {
        Self {
            refresh_compatible_components: BTreeSet::new(),
            legacy_manifest_requires_rebuild: true,
        }
    }
}

pub fn classify_analysis_semantics(
    stored: Option<&AnalysisSemanticsState>,
    current: &AnalysisSemanticsState,
) -> AnalysisSemanticsCompatibility {
    classify_analysis_semantics_with_policy(
        stored,
        current,
        &AnalysisSemanticsCompatibilityPolicy::default(),
    )
}

pub fn classify_analysis_semantics_with_policy(
    stored: Option<&AnalysisSemanticsState>,
    current: &AnalysisSemanticsState,
    policy: &AnalysisSemanticsCompatibilityPolicy,
) -> AnalysisSemanticsCompatibility {
    let Some(stored) = stored else {
        return AnalysisSemanticsCompatibility {
            status: if policy.legacy_manifest_requires_rebuild {
                AnalysisSemanticsCompatibilityStatus::RebuildRequired
            } else {
                AnalysisSemanticsCompatibilityStatus::RefreshRequired
            },
            stored_fingerprint: None,
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec!["legacy index has no analysis-semantics descriptor".into()],
            affected_components: vec!["analysis_semantics".into()],
            affected_languages: Vec::new(),
            recommended_action:
                "run `ok index .` to rebuild the index with current analysis semantics".into(),
        };
    };

    if stored.descriptor.version > current.descriptor.version {
        return AnalysisSemanticsCompatibility {
            status: AnalysisSemanticsCompatibilityStatus::FutureUnsupported,
            stored_fingerprint: Some(stored.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec![format!(
                "stored descriptor version {} is newer than supported version {}",
                stored.descriptor.version, current.descriptor.version
            )],
            affected_components: vec!["descriptor_version".into()],
            affected_languages: Vec::new(),
            recommended_action: "upgrade Open Kioku before reading this index".into(),
        };
    }

    if !stored.fingerprint_is_valid() {
        return AnalysisSemanticsCompatibility {
            status: AnalysisSemanticsCompatibilityStatus::RebuildRequired,
            stored_fingerprint: Some(stored.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec!["stored analysis-semantics fingerprint does not match its descriptor".into()],
            affected_components: vec!["fingerprint_integrity".into()],
            affected_languages: Vec::new(),
            recommended_action: "run `ok index .` to rebuild the index; do not trust persisted relationship authority"
                .into(),
        };
    }

    if stored == current {
        return AnalysisSemanticsCompatibility::compatible(current);
    }

    let mut reasons = Vec::new();
    let mut components = BTreeSet::new();
    let mut languages = BTreeSet::new();

    compare_component(
        "descriptor_version",
        &stored.descriptor.version.to_string(),
        &current.descriptor.version.to_string(),
        &mut components,
        &mut reasons,
    );
    compare_component(
        "stable_identity",
        &stored.descriptor.stable_identity_version,
        &current.descriptor.stable_identity_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "project_resolver",
        &stored.descriptor.project_resolver_version,
        &current.descriptor.project_resolver_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "relationship_resolver",
        &stored.descriptor.relationship_resolver_version,
        &current.descriptor.relationship_resolver_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "proof_policy",
        &stored.descriptor.proof_policy_version,
        &current.descriptor.proof_policy_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "graph_emission",
        &stored.descriptor.graph_emission_version,
        &current.descriptor.graph_emission_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "exact_index_ingestion",
        &stored.descriptor.exact_index_ingestion_version,
        &current.descriptor.exact_index_ingestion_version,
        &mut components,
        &mut reasons,
    );
    compare_language_map(
        "parser_semantics",
        &stored.descriptor.parser_semantics,
        &current.descriptor.parser_semantics,
        &mut components,
        &mut languages,
        &mut reasons,
    );
    compare_language_map(
        "language_adapter",
        &stored.descriptor.language_adapter_versions,
        &current.descriptor.language_adapter_versions,
        &mut components,
        &mut languages,
        &mut reasons,
    );

    if components.is_empty() {
        // Descriptor equality with a different valid fingerprint cannot happen with the current
        // canonical encoding, so fail closed if a future encoding ever violates that invariant.
        components.insert("fingerprint".into());
        reasons.push(
            "analysis-semantics fingerprints differ without a classified component change".into(),
        );
    }

    let refresh_only = components
        .iter()
        .all(|component| policy.refresh_compatible_components.contains(component));
    let status = if refresh_only {
        AnalysisSemanticsCompatibilityStatus::RefreshRequired
    } else {
        AnalysisSemanticsCompatibilityStatus::RebuildRequired
    };
    AnalysisSemanticsCompatibility {
        status,
        stored_fingerprint: Some(stored.fingerprint.clone()),
        current_fingerprint: current.fingerprint.clone(),
        reasons,
        affected_components: components.into_iter().collect(),
        affected_languages: languages.into_iter().collect(),
        recommended_action: match status {
            AnalysisSemanticsCompatibilityStatus::RefreshRequired => {
                "refresh stale semantic components before relying on authoritative relationship evidence"
                    .into()
            }
            _ => "run `ok index .` to rebuild the index with current analysis semantics".into(),
        },
    }
}

fn compare_component(
    name: &str,
    stored: &str,
    current: &str,
    components: &mut BTreeSet<String>,
    reasons: &mut Vec<String>,
) {
    if stored != current {
        components.insert(name.into());
        reasons.push(format!("{name} changed from `{stored}` to `{current}`"));
    }
}

fn compare_language_map(
    component: &str,
    stored: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
    components: &mut BTreeSet<String>,
    languages: &mut BTreeSet<String>,
    reasons: &mut Vec<String>,
) {
    let keys = stored
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for language in keys {
        let before = stored.get(&language);
        let after = current.get(&language);
        if before != after {
            components.insert(format!("{component}:{language}"));
            languages.insert(language.clone());
            reasons.push(format!(
                "{component} for {language} changed from `{}` to `{}`",
                before.map(String::as_str).unwrap_or("<missing>"),
                after.map(String::as_str).unwrap_or("<missing>")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_across_map_insertion_order() {
        let first = current_analysis_semantics_descriptor();
        let mut second = first.clone();
        second.parser_semantics = first
            .parser_semantics
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        second.language_adapter_versions = first
            .language_adapter_versions
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn identical_semantics_are_compatible() {
        let state = AnalysisSemanticsState::current();
        let compatibility = classify_analysis_semantics(Some(&state), &state);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::Compatible
        );
    }

    #[test]
    fn legacy_manifest_fails_closed() {
        let current = AnalysisSemanticsState::current();
        let compatibility = classify_analysis_semantics(None, &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
    }

    #[test]
    fn proof_policy_change_requires_rebuild() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.proof_policy_version = "old-policy".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(compatibility.affected_components, vec!["proof_policy"]);
    }

    #[test]
    fn relationship_resolver_change_requires_rebuild() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.relationship_resolver_version = "old-resolver".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(
            compatibility.affected_components,
            vec!["relationship_resolver"]
        );
    }

    #[test]
    fn one_language_adapter_change_is_scoped() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored
            .descriptor
            .language_adapter_versions
            .insert("java".into(), "old-java-adapter".into());
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(compatibility.affected_languages, vec!["java"]);
        assert_eq!(
            compatibility.affected_components,
            vec!["language_adapter:java"]
        );
    }

    #[test]
    fn future_descriptor_is_unsupported() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.version += 1;
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::FutureUnsupported
        );
    }

    #[test]
    fn explicit_refresh_policy_can_classify_refresh_required() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.project_resolver_version = "old-project-resolver".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let policy = AnalysisSemanticsCompatibilityPolicy {
            refresh_compatible_components: BTreeSet::from(["project_resolver".into()]),
            legacy_manifest_requires_rebuild: true,
        };
        let compatibility =
            classify_analysis_semantics_with_policy(Some(&stored), &current, &policy);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RefreshRequired
        );
    }
}
