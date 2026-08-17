mod bare_calls;
pub mod calls;
pub mod context;
pub mod evidence;
pub mod index;
pub mod inheritance;
pub mod language_capabilities;
pub mod pipeline;
mod self_calls;
mod type_relations;
mod typed_calls;

pub use calls::{resolve_call, resolve_call_outcome};
pub use context::{ResolutionContext, ResolutionResult, UnresolvedReason};
pub use evidence::{ResolutionEvidence, ResolutionEvidenceKind, ResolvedRelationship};
pub use index::{BindingIndex, ScopeIndex, SymbolIndex};
pub use inheritance::InheritanceIndex;
pub use language_capabilities::{
    semantic_capabilities_for, CapabilityState, LanguageSemanticCapabilities, SemanticCapability,
    LANGUAGE_SEMANTIC_CAPABILITY_VERSION,
};
pub use pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
};
pub use type_relations::{
    resolve_declared_type_use_outcome, resolve_inheritance_relationship_outcome,
};

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Binding, BindingId, CallSite, CallSiteId, Confidence, EvidenceSourceType, FileId, Language,
        LineRange, ReceiverKind, RelationshipAuthority, Scope, ScopeId, ScopeKind, SourceRange,
        Symbol, SymbolId, SymbolKind, Visibility,
    };

    #[test]
    fn resolves_same_name_method_via_typed_receiver() {
        let repo_class = Symbol {
            id: SymbolId::new("symbol:Repo"),
            name: "Repo".into(),
            qualified_name: "com.acme.Repo".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("file:Repo.java"),
            range: Some(LineRange { start: 1, end: 20 }),
            language: Language::Java,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        };

        let repo_save = Symbol {
            id: SymbolId::new("symbol:Repo.save"),
            name: "save".into(),
            qualified_name: "com.acme.Repo.save".into(),
            kind: SymbolKind::Method,
            file_id: FileId::new("file:Repo.java"),
            range: Some(LineRange { start: 5, end: 10 }),
            language: Language::Java,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: Some(SymbolId::new("symbol:Repo")),
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        };

        let sym_index = SymbolIndex::build(vec![repo_class, repo_save]);

        let file_scope = Scope {
            id: ScopeId::new("scope:file"),
            file_id: FileId::new("file:Main.java"),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::File,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 50,
                end_column: 1,
            },
        };
        let scope_index = ScopeIndex::build(vec![file_scope]);

        let binding = Binding {
            id: BindingId::new("binding:repo"),
            file_id: FileId::new("file:Main.java"),
            scope_id: ScopeId::new("scope:file"),
            name: "repo".into(),
            declared_type: Some("Repo".into()),
            inferred_type: None,
            range: SourceRange {
                start_line: 5,
                start_column: 1,
                end_line: 5,
                end_column: 20,
            },
        };
        let binding_index = BindingIndex::build(vec![binding]);
        let inheritance_index = InheritanceIndex::build(vec![]);
        let mut repository = open_kioku_semantic_model::SemanticRepository::new();
        repository
            .imports
            .insert(open_kioku_semantic_model::ImportBinding {
                file_id: FileId::new("file:Main.java"),
                scope_id: ScopeId::new("scope:file"),
                local_name: "Repo".into(),
                imported_name: "Repo".into(),
                source_module: "com.acme.Repo".into(),
                resolved_module: None,
                target_file: Some(FileId::new("file:Repo.java")),
                target_symbol: Some(SymbolId::new("symbol:Repo")),
                origin: open_kioku_semantic_model::ImportOrigin::Internal,
                is_type_only: false,
                is_glob: false,
                evidence: Vec::new(),
            });
        let semantics = open_kioku_languages::semantics_for(&Language::Java).unwrap();
        let main_file_id = FileId::new("file:Main.java");

        let file_path = std::path::Path::new("src/Main.java");
        let ctx = ResolutionContext::new(
            &main_file_id,
            file_path,
            None,
            Language::Java,
            &repository,
            &sym_index,
            &scope_index,
            &binding_index,
            &inheritance_index,
            semantics,
        );

        let call = CallSite {
            id: CallSiteId::new("call:save"),
            file_id: FileId::new("file:Main.java"),
            scope_id: ScopeId::new("scope:file"),
            caller_symbol_id: None,
            callee_name: "save".into(),
            receiver: Some("repo".into()),
            receiver_kind: ReceiverKind::Value,
            range: SourceRange {
                start_line: 10,
                start_column: 5,
                end_line: 10,
                end_column: 15,
            },
        };

        let outcome = resolve_call_outcome(&call, &ctx);
        match outcome {
            ResolutionOutcome::Proven { candidate } => {
                assert_eq!(
                    candidate.target_symbol_id,
                    SymbolId::new("symbol:Repo.save")
                );
                assert_eq!(
                    candidate.authority(&open_kioku_core::GraphEdgeType::Calls),
                    RelationshipAuthority::Authoritative
                );
                assert!(!candidate.proofs.is_empty());
            }
            other => panic!("expected proven call outcome, got {other:?}"),
        }

        let res = resolve_call(&call, &ctx);
        match res {
            ResolutionResult::Resolved { target, .. } => {
                assert_eq!(target, SymbolId::new("symbol:Repo.save"));
            }
            _ => panic!("Expected resolved call edge"),
        }
    }
}
