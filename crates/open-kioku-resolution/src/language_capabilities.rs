use open_kioku_core::Language;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of the built-in language-semantic capability contract.
///
/// This is intentionally independent from storage schema versions. A future change that alters
/// the meaning of a capability should bump this value and, once RI3 analysis-semantics
/// fingerprinting lands, feed that fingerprint rather than silently changing index meaning.
pub const LANGUAGE_SEMANTIC_CAPABILITY_VERSION: u32 = 1;

/// Semantic relationship features that a language adapter may support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCapability {
    CallsLocalFunction,
    CallsImportedFunction,
    CallsInstanceMember,
    CallsStaticMember,
    CallsDynamicDispatch,
    ReferencesImportBinding,
    TypesAnnotation,
    TypesConstructor,
    InheritanceExtends,
    InheritanceImplements,
    SourceRangesCallSite,
}

/// Declared support level for one semantic capability.
///
/// This descriptor is diagnostic metadata only. `SupportedAuthoritative` means the current
/// resolver has a proof-producing path for the capability; it does not itself authorize any
/// relationship. Individual relationships must still satisfy the centralized proof policy and
/// uniqueness checks in `evaluate_candidates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    SupportedAuthoritative,
    SupportedCorroborating,
    Unsupported,
}

/// Versioned, machine-readable semantic capability descriptor for a language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageSemanticCapabilities {
    pub version: u32,
    pub language: Language,
    pub capabilities: BTreeMap<SemanticCapability, CapabilityState>,
}

impl LanguageSemanticCapabilities {
    /// Return the declared state of a capability, failing closed for absent entries.
    pub fn state(&self, capability: SemanticCapability) -> CapabilityState {
        self.capabilities
            .get(&capability)
            .copied()
            .unwrap_or(CapabilityState::Unsupported)
    }

    /// Whether the descriptor claims a proof-producing authoritative path exists.
    ///
    /// This must never be used as a substitute for relationship-level proof evaluation.
    pub fn has_authoritative_path(&self, capability: SemanticCapability) -> bool {
        self.state(capability) == CapabilityState::SupportedAuthoritative
    }
}

/// Return the built-in semantic capability descriptor for a Tier-1 language.
///
/// The matrix is deliberately conservative: dynamic dispatch remains unsupported, and language
/// features whose target cannot be uniquely proven from currently indexed facts are only marked
/// corroborating or unsupported. Unsupported languages return `None` rather than inheriting a
/// generic optimistic profile.
pub fn semantic_capabilities_for(language: &Language) -> Option<LanguageSemanticCapabilities> {
    use CapabilityState::{
        SupportedAuthoritative as Authoritative, SupportedCorroborating as Corroborating,
        Unsupported,
    };
    use SemanticCapability::*;

    let entries: &[(SemanticCapability, CapabilityState)] = match language {
        Language::Rust => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Corroborating),
            (CallsStaticMember, Authoritative),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Corroborating),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Unsupported),
            (InheritanceImplements, Corroborating),
            (SourceRangesCallSite, Authoritative),
        ],
        Language::TypeScript => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Corroborating),
            (CallsStaticMember, Corroborating),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Corroborating),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Corroborating),
            (InheritanceImplements, Corroborating),
            (SourceRangesCallSite, Authoritative),
        ],
        Language::JavaScript => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Corroborating),
            (CallsStaticMember, Corroborating),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Unsupported),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Corroborating),
            (InheritanceImplements, Unsupported),
            (SourceRangesCallSite, Authoritative),
        ],
        Language::Python => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Corroborating),
            (CallsStaticMember, Corroborating),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Corroborating),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Corroborating),
            (InheritanceImplements, Unsupported),
            (SourceRangesCallSite, Authoritative),
        ],
        Language::Java => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Authoritative),
            (CallsStaticMember, Authoritative),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Authoritative),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Corroborating),
            (InheritanceImplements, Corroborating),
            (SourceRangesCallSite, Authoritative),
        ],
        Language::Go => &[
            (CallsLocalFunction, Authoritative),
            (CallsImportedFunction, Authoritative),
            (CallsInstanceMember, Authoritative),
            (CallsStaticMember, Authoritative),
            (CallsDynamicDispatch, Unsupported),
            (ReferencesImportBinding, Authoritative),
            (TypesAnnotation, Authoritative),
            (TypesConstructor, Corroborating),
            (InheritanceExtends, Unsupported),
            (InheritanceImplements, Unsupported),
            (SourceRangesCallSite, Authoritative),
        ],
        _ => return None,
    };

    Some(LanguageSemanticCapabilities {
        version: LANGUAGE_SEMANTIC_CAPABILITY_VERSION,
        language: language.clone(),
        capabilities: entries.iter().copied().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_one_languages_have_complete_deterministic_descriptors() {
        let languages = [
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Java,
            Language::Go,
        ];

        for language in languages {
            let descriptor = semantic_capabilities_for(&language).expect("Tier-1 descriptor");
            assert_eq!(descriptor.version, LANGUAGE_SEMANTIC_CAPABILITY_VERSION);
            assert_eq!(descriptor.language, language);
            assert_eq!(descriptor.capabilities.len(), 11);
            assert_eq!(
                descriptor.state(SemanticCapability::CallsDynamicDispatch),
                CapabilityState::Unsupported,
                "dynamic dispatch must fail closed for {language:?}"
            );
            assert_eq!(
                descriptor.state(SemanticCapability::SourceRangesCallSite),
                CapabilityState::SupportedAuthoritative
            );
        }
    }

    #[test]
    fn javascript_does_not_inherit_typescript_type_claims() {
        let javascript = semantic_capabilities_for(&Language::JavaScript).unwrap();
        let typescript = semantic_capabilities_for(&Language::TypeScript).unwrap();

        assert_eq!(
            javascript.state(SemanticCapability::TypesAnnotation),
            CapabilityState::Unsupported
        );
        assert_ne!(
            javascript.state(SemanticCapability::TypesAnnotation),
            typescript.state(SemanticCapability::TypesAnnotation)
        );
    }

    #[test]
    fn unsupported_languages_do_not_receive_optimistic_defaults() {
        assert!(semantic_capabilities_for(&Language::Unknown).is_none());
    }

    #[test]
    fn serialized_descriptor_is_stable_and_machine_readable() {
        let descriptor = semantic_capabilities_for(&Language::Java).unwrap();
        let json = serde_json::to_string(&descriptor).unwrap();
        assert!(json.contains("\"calls_instance_member\":\"supported_authoritative\""));
        assert!(json.contains("\"calls_dynamic_dispatch\":\"unsupported\""));
    }
}
