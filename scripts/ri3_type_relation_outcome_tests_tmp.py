from pathlib import Path

path = Path("crates/open-kioku-resolution/src/type_relations.rs")
text = path.read_text()
text += r'''

#[cfg(test)]
mod ri3_relationship_outcome_tests {
    use super::*;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        BindingId, FileId, Language, Scope, ScopeKind, SourceRange, Symbol, Visibility,
    };
    use open_kioku_semantic_model::SemanticRepository;

    fn symbol(id: &str, name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{name}"),
            kind,
            file_id: FileId::new("file:src/lib.rs"),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: Some(ScopeId::new("scope:fn")),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn range() -> SourceRange {
        SourceRange {
            start_line: 7,
            start_column: 3,
            end_line: 7,
            end_column: 21,
        }
    }

    struct Fixture {
        symbols: SymbolIndex,
        scopes: ScopeIndex,
        bindings: BindingIndex,
        inheritance: InheritanceIndex,
        repo: SemanticRepository,
        file_id: FileId,
    }

    impl Fixture {
        fn new(symbols: Vec<Symbol>, scopes: Vec<Scope>, bindings: Vec<Binding>) -> Self {
            Self {
                symbols: SymbolIndex::build(symbols),
                scopes: ScopeIndex::build(scopes),
                bindings: BindingIndex::build(bindings),
                inheritance: InheritanceIndex::default(),
                repo: SemanticRepository::new(),
                file_id: FileId::new("file:src/lib.rs"),
            }
        }

        fn context(&self) -> ResolutionContext<'_> {
            ResolutionContext::new(
                &self.file_id,
                std::path::Path::new("src/lib.rs"),
                None,
                Language::Rust,
                &self.repo,
                &self.symbols,
                &self.scopes,
                &self.bindings,
                &self.inheritance,
                open_kioku_languages::semantics_for(&Language::Rust).unwrap(),
            )
        }
    }

    fn proof_kinds(candidate: &ResolutionCandidate) -> BTreeSet<RelationshipProofKind> {
        candidate.proofs.iter().map(|proof| proof.kind).collect()
    }

    #[test]
    fn unique_same_file_extends_is_proven_with_declaration_and_binding_proofs() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let parent = symbol("symbol:parent", "Parent", SymbolKind::Class);
        let fixture = Fixture::new(vec![child.clone(), parent.clone()], Vec::new(), Vec::new());
        let site = InheritanceSite {
            child_symbol_id: child.id.clone(),
            parent_name: "Parent".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(),
        };

        let (edge_type, outcome) =
            resolve_inheritance_relationship_outcome(&site, &fixture.context());
        assert_eq!(edge_type, GraphEdgeType::Extends);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique exact parent should prove EXTENDS");
        };
        assert_eq!(candidate.target_symbol_id, parent.id);
        let kinds = proof_kinds(&candidate);
        assert!(kinds.contains(&RelationshipProofKind::InheritanceBinding));
        assert!(kinds.contains(&RelationshipProofKind::SameScopeDefinition));
        assert!(candidate.proofs.iter().all(|proof| proof.source_range.is_some()));
    }

    #[test]
    fn unique_trait_implements_is_proven_with_trait_binding() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let tr = symbol("symbol:trait", "Runnable", SymbolKind::Trait);
        let fixture = Fixture::new(vec![child.clone(), tr.clone()], Vec::new(), Vec::new());
        let site = InheritanceSite {
            child_symbol_id: child.id.clone(),
            parent_name: "Runnable".into(),
            kind: InheritanceKind::Implements,
            order: 0,
            range: range(),
        };

        let (edge_type, outcome) =
            resolve_inheritance_relationship_outcome(&site, &fixture.context());
        assert_eq!(edge_type, GraphEdgeType::Implements);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique exact trait should prove IMPLEMENTS");
        };
        assert_eq!(candidate.target_symbol_id, tr.id);
        let kinds = proof_kinds(&candidate);
        assert!(kinds.contains(&RelationshipProofKind::InheritanceBinding));
        assert!(kinds.contains(&RelationshipProofKind::TraitOrInterfaceBinding));
    }

    #[test]
    fn unique_explicit_declared_type_is_proven_as_uses_type() {
        let owner = symbol("symbol:owner", "owner", SymbolKind::Function);
        let ty = symbol("symbol:thing", "Thing", SymbolKind::Class);
        let scope = Scope {
            id: ScopeId::new("scope:fn"),
            file_id: FileId::new("file:src/lib.rs"),
            parent_id: None,
            owner_symbol_id: Some(owner.id.clone()),
            kind: ScopeKind::Function,
            range: range(),
        };
        let binding = Binding {
            id: BindingId::new("binding:value"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: scope.id.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: Some("WrongInferredType".into()),
            range: range(),
        };
        let fixture = Fixture::new(
            vec![owner.clone(), ty.clone()],
            vec![scope],
            vec![binding.clone()],
        );

        let (source, outcome) =
            resolve_declared_type_use_outcome(&binding, &fixture.context()).unwrap();
        assert_eq!(source, owner.id);
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("unique explicit declared type should prove USES_TYPE");
        };
        assert_eq!(candidate.target_symbol_id, ty.id);
        assert_eq!(proof_kinds(&candidate), BTreeSet::from([RelationshipProofKind::ExactReference]));
        assert!(candidate.proofs.iter().all(|proof| proof.source_range.is_some()));
    }

    #[test]
    fn inheritance_outcome_is_identical_under_reversed_symbol_insertion_order() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class);
        let first = symbol("symbol:a", "Parent", SymbolKind::Class);
        let second = symbol("symbol:b", "Parent", SymbolKind::Class);
        let forward = Fixture::new(
            vec![child.clone(), first.clone(), second.clone()],
            Vec::new(),
            Vec::new(),
        );
        let reversed = Fixture::new(
            vec![second, first, child.clone()],
            Vec::new(),
            Vec::new(),
        );
        let site = InheritanceSite {
            child_symbol_id: child.id,
            parent_name: "Parent".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(),
        };

        let (_, left) = resolve_inheritance_relationship_outcome(&site, &forward.context());
        let (_, right) = resolve_inheritance_relationship_outcome(&site, &reversed.context());
        match (left, right) {
            (
                ResolutionOutcome::Ambiguous { candidates: left, .. },
                ResolutionOutcome::Ambiguous { candidates: right, .. },
            ) => {
                let left_ids = left
                    .into_iter()
                    .map(|candidate| candidate.target_symbol_id)
                    .collect::<Vec<_>>();
                let right_ids = right
                    .into_iter()
                    .map(|candidate| candidate.target_symbol_id)
                    .collect::<Vec<_>>();
                assert_eq!(left_ids, right_ids);
                assert_eq!(
                    left_ids,
                    vec![SymbolId::new("symbol:a"), SymbolId::new("symbol:b")]
                );
            }
            other => panic!("expected deterministic ambiguity, got {other:?}"),
        }
    }
}
'''
path.write_text(text)
