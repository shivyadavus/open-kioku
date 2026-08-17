use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::{ScopeIndex, SymbolIndex};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    Binding, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, InheritanceKind,
    InheritanceSite, LineRange, RelationshipProof, RelationshipProofKind, ScopeId, Symbol,
    SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ParentBindingKind {
    SameFile,
    Import,
    QualifiedName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentTypeCandidate {
    pub target: SymbolId,
    pub bindings: BTreeSet<ParentBindingKind>,
}

pub(crate) fn collect_parent_type_candidates(
    child: &Symbol,
    parent_name: &str,
    symbols: &SymbolIndex,
    repository: &SemanticRepository,
) -> Vec<ParentTypeCandidate> {
    let mut candidates = BTreeMap::<String, ParentTypeCandidate>::new();
    let mut add = |target: SymbolId, binding: ParentBindingKind| {
        let entry = candidates
            .entry(target.0.clone())
            .or_insert_with(|| ParentTypeCandidate {
                target,
                bindings: BTreeSet::new(),
            });
        entry.bindings.insert(binding);
    };

    if let Some(file_symbols) = symbols.by_file.get(&child.file_id) {
        for id in file_symbols {
            if symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == parent_name)
                .unwrap_or(false)
            {
                add(id.clone(), ParentBindingKind::SameFile);
            }
        }
    }

    for binding in repository.imports.lookup(&child.file_id, None, parent_name) {
        if let Some(target) = &binding.target_symbol {
            if symbols
                .get(target)
                .map(|symbol| is_type_symbol(&symbol.kind))
                .unwrap_or(false)
            {
                add(target.clone(), ParentBindingKind::Import);
            }
        }
        if let Some(target_file) = &binding.target_file {
            if let Some(file_symbols) = symbols.by_file.get(target_file) {
                for id in file_symbols {
                    if symbols
                        .get(id)
                        .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == parent_name)
                        .unwrap_or(false)
                    {
                        add(id.clone(), ParentBindingKind::Import);
                    }
                }
            }
        }
    }

    if let Some(qualified) = symbols.by_qualified.get(parent_name) {
        for id in qualified {
            if symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind))
                .unwrap_or(false)
            {
                add(id.clone(), ParentBindingKind::QualifiedName);
            }
        }
    }

    candidates.into_values().collect()
}

pub fn resolve_inheritance_relationship_outcome(
    site: &InheritanceSite,
    ctx: &ResolutionContext<'_>,
) -> (GraphEdgeType, ResolutionOutcome) {
    let edge_type = inheritance_edge_type(&site.kind);
    let Some(child) = ctx.symbols.get(&site.child_symbol_id) else {
        return (
            edge_type.clone(),
            evaluate_candidates(&edge_type, Vec::new()),
        );
    };
    let parent_candidates =
        collect_parent_type_candidates(child, &site.parent_name, ctx.symbols, ctx.repository);
    let target_ids = parent_candidates
        .iter()
        .map(|candidate| candidate.target.clone())
        .collect::<Vec<_>>();
    let candidate_count = target_ids.len();
    let ambiguity = ambiguity_strings(&target_ids);
    let source_range = syntax_file_range(ctx, &site.range);

    let candidates = parent_candidates
        .into_iter()
        .map(|parent| {
            let mut candidate = ResolutionCandidate::new(parent.target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::InheritanceGraph,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: source_range.clone(),
                symbol_id: Some(parent.target.clone()),
                message: format!(
                    "explicit {:?} declaration candidate for {}",
                    site.kind, site.parent_name
                ),
            });
            candidate.proofs.push(proof(
                RelationshipProofKind::InheritanceBinding,
                "explicit_inheritance_declaration",
                source_range.clone(),
                &site.child_symbol_id,
                &parent.target,
                candidate_count,
                &ambiguity,
            ));
            for binding in parent.bindings {
                let (kind, strategy) = match binding {
                    ParentBindingKind::SameFile => (
                        RelationshipProofKind::SameScopeDefinition,
                        "same_file_parent_type",
                    ),
                    ParentBindingKind::Import => (
                        RelationshipProofKind::ImportBinding,
                        "import_bound_parent_type",
                    ),
                    ParentBindingKind::QualifiedName => (
                        RelationshipProofKind::QualifiedName,
                        "qualified_parent_type",
                    ),
                };
                candidate.proofs.push(proof(
                    kind,
                    strategy,
                    source_range.clone(),
                    &site.child_symbol_id,
                    &parent.target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            if matches!(
                site.kind,
                InheritanceKind::Implements | InheritanceKind::TraitImpl
            ) && ctx
                .symbols
                .get(&parent.target)
                .map(|target| matches!(target.kind, SymbolKind::Trait | SymbolKind::Interface))
                .unwrap_or(false)
            {
                candidate.proofs.push(proof(
                    RelationshipProofKind::TraitOrInterfaceBinding,
                    "trait_or_interface_target",
                    source_range.clone(),
                    &site.child_symbol_id,
                    &parent.target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();

    (
        edge_type.clone(),
        evaluate_candidates(&edge_type, candidates),
    )
}

pub fn resolve_declared_type_use_outcome(
    binding: &Binding,
    ctx: &ResolutionContext<'_>,
) -> Option<(SymbolId, ResolutionOutcome)> {
    let type_name = binding.declared_type.as_deref()?.trim();
    if type_name.is_empty() {
        return None;
    }
    let source = scope_owner_symbol(&binding.scope_id, ctx.scopes)?;
    let targets = crate::typed_calls::collect_type_candidates(ctx, &binding.scope_id, type_name);
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let source_range = syntax_file_range(ctx, &binding.range);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: source_range.clone(),
                symbol_id: Some(target.clone()),
                message: format!("explicit declared type `{type_name}` candidate"),
            });
            candidate.proofs.push(proof(
                RelationshipProofKind::ExactReference,
                "explicit_declared_type",
                source_range.clone(),
                &source,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    Some((
        source,
        evaluate_candidates(&GraphEdgeType::UsesType, candidates),
    ))
}

pub(crate) fn scope_owner_symbol(scope_id: &ScopeId, scopes: &ScopeIndex) -> Option<SymbolId> {
    let mut current = Some(scope_id.clone());
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            return None;
        }
        let scope = scopes.get(&id)?;
        if let Some(owner) = &scope.owner_symbol_id {
            return Some(owner.clone());
        }
        current = scope.parent_id.clone();
    }
    None
}

fn inheritance_edge_type(kind: &InheritanceKind) -> GraphEdgeType {
    match kind {
        InheritanceKind::Extends => GraphEdgeType::Extends,
        InheritanceKind::Implements | InheritanceKind::TraitImpl => GraphEdgeType::Implements,
        InheritanceKind::Embeds => GraphEdgeType::UsesType,
    }
}

fn proof(
    kind: RelationshipProofKind,
    strategy: &str,
    source_range: Option<FileRange>,
    source: &SymbolId,
    target: &SymbolId,
    candidate_count: usize,
    ambiguity: &[String],
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = source_range;
    proof.source_symbol_id = Some(source.clone());
    proof.target_symbol_id = Some(target.clone());
    proof.ambiguity = ambiguity.to_vec();
    proof
}

fn syntax_file_range(
    ctx: &ResolutionContext<'_>,
    range: &open_kioku_core::SourceRange,
) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: range.start_line,
            end: range.end_line,
        }),
    })
}

fn ambiguity_strings(ids: &[SymbolId]) -> Vec<String> {
    if ids.len() > 1 {
        ids.iter().map(|id| id.0.clone()).collect()
    } else {
        Vec::new()
    }
}

fn is_type_symbol(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        BindingId, FileId, Language, Scope, ScopeKind, SourceRange, Symbol, Visibility,
    };
    use open_kioku_semantic_model::SemanticRepository;

    fn symbol(id: &str, name: &str, kind: SymbolKind, parent: Option<&str>) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind,
            file_id: FileId::new("file:src/lib.rs"),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: parent.map(SymbolId::new),
            scope_id: Some(ScopeId::new("scope:fn")),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn range() -> SourceRange {
        SourceRange {
            start_line: 3,
            start_column: 1,
            end_line: 3,
            end_column: 20,
        }
    }

    #[test]
    fn duplicate_same_file_parent_names_remain_ambiguous() {
        let child = symbol("symbol:child", "Child", SymbolKind::Class, None);
        let first = symbol("symbol:a", "Parent", SymbolKind::Class, None);
        let second = symbol("symbol:b", "Parent", SymbolKind::Class, None);
        let forward = SymbolIndex::build(vec![child.clone(), first.clone(), second.clone()]);
        let reversed = SymbolIndex::build(vec![second, first, child.clone()]);
        let repo = SemanticRepository::new();
        let left = collect_parent_type_candidates(&child, "Parent", &forward, &repo);
        let right = collect_parent_type_candidates(&child, "Parent", &reversed, &repo);
        assert_eq!(left, right);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].target.0, "symbol:a");
        assert_eq!(left[1].target.0, "symbol:b");
    }

    #[test]
    fn declared_type_requires_unique_exact_target() {
        let owner = symbol("symbol:owner", "owner", SymbolKind::Function, None);
        let ty_a = symbol("symbol:a", "Thing", SymbolKind::Class, None);
        let ty_b = symbol("symbol:b", "Thing", SymbolKind::Class, None);
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
            inferred_type: None,
            range: range(),
        };
        let symbols = SymbolIndex::build(vec![owner, ty_a, ty_b]);
        let scopes = ScopeIndex::build(vec![scope]);
        let bindings = BindingIndex::build(vec![binding.clone()]);
        let inheritance = InheritanceIndex::default();
        let repo = SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let file_id = FileId::new("file:src/lib.rs");
        let ctx = ResolutionContext::new(
            &file_id,
            std::path::Path::new("src/lib.rs"),
            None,
            Language::Rust,
            &repo,
            &symbols,
            &scopes,
            &bindings,
            &inheritance,
            semantics,
        );
        let (_, outcome) = resolve_declared_type_use_outcome(&binding, &ctx).unwrap();
        assert!(matches!(outcome, ResolutionOutcome::Ambiguous { .. }));
    }
}

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
        assert!(candidate
            .proofs
            .iter()
            .all(|proof| proof.source_range.is_some()));
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
        assert_eq!(
            proof_kinds(&candidate),
            BTreeSet::from([RelationshipProofKind::ExactReference])
        );
        assert!(candidate
            .proofs
            .iter()
            .all(|proof| proof.source_range.is_some()));
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
        let reversed = Fixture::new(vec![second, first, child.clone()], Vec::new(), Vec::new());
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
                ResolutionOutcome::Ambiguous {
                    candidates: left, ..
                },
                ResolutionOutcome::Ambiguous {
                    candidates: right, ..
                },
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
