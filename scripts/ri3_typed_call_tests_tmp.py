from pathlib import Path

path = Path("crates/open-kioku-resolution/src/typed_calls.rs")
text = path.read_text()
if "mod tests {" in text:
    raise SystemExit("typed-call tests already present")

tests = r'''

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ResolutionContext;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        Binding, BindingId, CallSiteId, FileId, Language, ReceiverKind, Scope, ScopeKind,
        SourceRange, Symbol, Visibility,
    };

    fn type_symbol(id: &str, name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind: SymbolKind::Class,
            file_id: FileId::new("file:src/lib.rs"),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: Some(ScopeId::new("scope:file")),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn method_symbol(id: &str, parent: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: "run".into(),
            qualified_name: format!("pkg::{parent}::run"),
            kind: SymbolKind::Method,
            file_id: FileId::new("file:src/lib.rs"),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: Some(SymbolId::new(parent)),
            scope_id: Some(ScopeId::new("scope:file")),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn call() -> CallSite {
        CallSite {
            id: CallSiteId::new("call:svc.run"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: ScopeId::new("scope:file"),
            caller_symbol_id: Some(SymbolId::new("symbol:caller")),
            callee_name: "run".into(),
            receiver: Some("svc".into()),
            receiver_kind: ReceiverKind::Value,
            range: SourceRange {
                start_line: 20,
                start_column: 5,
                end_line: 20,
                end_column: 14,
            },
        }
    }

    fn with_context<T>(symbols: Vec<Symbol>, test: impl FnOnce(&ResolutionContext<'_>) -> T) -> T {
        let file_id = FileId::new("file:src/lib.rs");
        let scopes = ScopeIndex::build(vec![Scope {
            id: ScopeId::new("scope:file"),
            file_id: file_id.clone(),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::File,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 100,
                end_column: 1,
            },
        }]);
        let bindings = BindingIndex::build(vec![Binding {
            id: BindingId::new("binding:svc"),
            file_id: file_id.clone(),
            scope_id: ScopeId::new("scope:file"),
            name: "svc".into(),
            declared_type: Some("Service".into()),
            inferred_type: None,
            range: SourceRange {
                start_line: 10,
                start_column: 1,
                end_line: 10,
                end_column: 20,
            },
        }]);
        let symbol_index = SymbolIndex::build(symbols);
        let inheritance = InheritanceIndex::build(Vec::new());
        let repository = open_kioku_semantic_model::SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let context = ResolutionContext::new(
            &file_id,
            std::path::Path::new("src/lib.rs"),
            None,
            Language::Rust,
            &repository,
            &symbol_index,
            &scopes,
            &bindings,
            &inheritance,
            semantics,
        );
        test(&context)
    }

    #[test]
    fn unique_typed_receiver_direct_member_is_proven() {
        with_context(
            vec![
                type_symbol("symbol:type:Service", "Service"),
                method_symbol("symbol:method:Service.run", "symbol:type:Service"),
            ],
            |ctx| match resolve_typed_receiver_outcome(&call(), ctx) {
                ResolutionOutcome::Proven { candidate } => {
                    assert_eq!(candidate.target_symbol_id.0, "symbol:method:Service.run");
                    assert!(candidate
                        .proofs
                        .iter()
                        .any(|proof| proof.kind == RelationshipProofKind::ExactCallSite));
                    assert!(candidate
                        .proofs
                        .iter()
                        .any(|proof| proof.kind == RelationshipProofKind::ReceiverType));
                    assert!(candidate
                        .proofs
                        .iter()
                        .any(|proof| proof.kind == RelationshipProofKind::ContainingType));
                }
                other => panic!("expected proven typed call, got {other:?}"),
            },
        );
    }

    #[test]
    fn duplicate_receiver_types_with_same_member_are_ambiguous_and_order_independent() {
        let symbols = vec![
            type_symbol("symbol:type:a:Service", "Service"),
            method_symbol("symbol:method:a.run", "symbol:type:a:Service"),
            type_symbol("symbol:type:b:Service", "Service"),
            method_symbol("symbol:method:b.run", "symbol:type:b:Service"),
        ];
        let first = with_context(symbols.clone(), |ctx| {
            resolve_typed_receiver_outcome(&call(), ctx)
        });
        let mut reversed = symbols;
        reversed.reverse();
        let second = with_context(reversed, |ctx| resolve_typed_receiver_outcome(&call(), ctx));

        let extract = |outcome: ResolutionOutcome| match outcome {
            ResolutionOutcome::Ambiguous { candidates, .. } => candidates
                .into_iter()
                .map(|candidate| candidate.target_symbol_id.0)
                .collect::<Vec<_>>(),
            other => panic!("expected ambiguous typed call, got {other:?}"),
        };
        assert_eq!(
            extract(first),
            vec!["symbol:method:a.run".to_string(), "symbol:method:b.run".to_string()]
        );
        assert_eq!(
            extract(second),
            vec!["symbol:method:a.run".to_string(), "symbol:method:b.run".to_string()]
        );
    }
}
'''
path.write_text(text + tests)
