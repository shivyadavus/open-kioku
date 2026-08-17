use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, Language, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
};

pub(crate) fn resolve_bare_call_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    // A lexical value binding with the same name shadows function/import symbols. Until a
    // callable value binding can itself be proven to a unique target, fail closed rather than
    // emitting a structural CALLS edge to an unrelated same-named symbol.
    if ctx
        .bindings
        .resolve_before(&call.scope_id, &call.callee_name, &call.range, ctx.scopes)
        .is_some()
    {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    if let Some(candidates) = nearest_lexical_scope_candidates(call, ctx) {
        return evaluate_target_set(
            call,
            ctx,
            candidates,
            Confidence::Exact,
            ResolutionEvidenceKind::LexicalScope,
            "lexical_scope",
            "bare call candidate from nearest lexical scope",
            &[RelationshipProofKind::SameScopeDefinition],
        );
    }

    if ctx.semantics.implicit_self_dispatch() {
        if let Some(caller_id) = &call.caller_symbol_id {
            if let Some(parent_id) = ctx
                .symbols
                .get(caller_id)
                .and_then(|caller| caller.parent_symbol_id.as_ref())
            {
                let self_members = find_members_by_name(ctx, parent_id, &call.callee_name);
                if !self_members.is_empty() {
                    return evaluate_target_set(
                        call,
                        ctx,
                        self_members,
                        Confidence::Exact,
                        ResolutionEvidenceKind::ImplicitSelf,
                        "implicit_self",
                        "bare call candidate from implicit self dispatch",
                        &[
                            RelationshipProofKind::ReceiverType,
                            RelationshipProofKind::ContainingType,
                        ],
                    );
                }

                // The current inheritance index exposes only one inherited winner. Retain it as a
                // corroborating candidate until the inheritance slice can expose the complete
                // candidate set; first-parent traversal must not become structural truth.
                if let Some(target) = ctx.inheritance.resolve_inherited_member(
                    parent_id,
                    &call.callee_name,
                    ctx.symbols,
                ) {
                    return evaluate_target_set(
                        call,
                        ctx,
                        vec![target],
                        Confidence::Exact,
                        ResolutionEvidenceKind::InheritanceGraph,
                        "implicit_self_inheritance_candidate",
                        "inherited bare-call candidate retained without authoritative uniqueness",
                        &[RelationshipProofKind::InheritanceBinding],
                    );
                }
            }
        }
    }

    let mut imported_targets = ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(&call.scope_id), &call.callee_name)
        .into_iter()
        .filter_map(|binding| binding.target_symbol.clone())
        .collect::<Vec<_>>();
    normalize_symbol_ids(&mut imported_targets);
    if !imported_targets.is_empty() {
        return evaluate_target_set(
            call,
            ctx,
            imported_targets,
            Confidence::Exact,
            ResolutionEvidenceKind::ExplicitImport,
            "explicit_import",
            "bare call candidate from exact import binding",
            &[
                RelationshipProofKind::ImportBinding,
                RelationshipProofKind::QualifiedName,
            ],
        );
    }

    if ctx.language != Language::Java {
        let mut same_file_candidates = ctx
            .symbols
            .by_file
            .get(ctx.file_id)
            .map(|symbols| symbols.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|symbol| {
                        symbol.name == call.callee_name
                            && symbol.parent_symbol_id.is_none()
                            && matches!(symbol.kind, SymbolKind::Function)
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        normalize_symbol_ids(&mut same_file_candidates);
        if !same_file_candidates.is_empty() {
            // Same-file simple-name matching is useful retrieval evidence, but it does not prove
            // lexical visibility or binding. ExactCallSite alone remains corroborating in core.
            return evaluate_target_set(
                call,
                ctx,
                same_file_candidates,
                Confidence::High,
                ResolutionEvidenceKind::SameFile,
                "same_file_candidate",
                "same-file bare-call candidate retained without binding proof",
                &[],
            );
        }
    }

    evaluate_candidates(&GraphEdgeType::Calls, Vec::new())
}

#[allow(clippy::too_many_arguments)]
fn evaluate_target_set(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    mut targets: Vec<SymbolId>,
    confidence: Confidence,
    evidence_kind: ResolutionEvidenceKind,
    strategy: &str,
    message: &str,
    target_proof_kinds: &[RelationshipProofKind],
) -> ResolutionOutcome {
    normalize_symbol_ids(&mut targets);
    let candidate_count = targets.len();
    let ambiguity = if candidate_count > 1 {
        targets.clone()
    } else {
        Vec::new()
    };
    let candidates = targets
        .iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), confidence);
            candidate.evidence.push(ResolutionEvidence {
                kind: evidence_kind.clone(),
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: message.into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, target));
            for kind in target_proof_kinds {
                candidate.proofs.push(target_proof(
                    *kind,
                    call,
                    ctx,
                    target,
                    strategy,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

fn call_site_proof(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(RelationshipProofKind::ExactCallSite, "call_site", 1);
    proof.source_range = call_file_range(call, ctx);
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof
}

#[allow(clippy::too_many_arguments)]
fn target_proof(
    kind: RelationshipProofKind,
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
    strategy: &str,
    candidate_count: usize,
    ambiguity: &[SymbolId],
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = call_file_range(call, ctx);
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof.ambiguity = ambiguity.iter().map(|id| id.0.clone()).collect();
    proof
}

fn nearest_lexical_scope_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Option<Vec<SymbolId>> {
    let mut current_scope_id = Some(call.scope_id.clone());
    let mut visited = std::collections::HashSet::new();
    while let Some(scope_id) = current_scope_id {
        if !visited.insert(scope_id.clone()) {
            break;
        }
        let mut candidates = ctx
            .symbols
            .by_file
            .get(ctx.file_id)
            .map(|symbols| symbols.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|symbol| {
                        symbol.name == call.callee_name
                            && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                            && symbol.scope_id.as_ref() == Some(&scope_id)
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        normalize_symbol_ids(&mut candidates);
        if !candidates.is_empty() {
            return Some(candidates);
        }
        current_scope_id = ctx
            .scopes
            .get(&scope_id)
            .and_then(|scope| scope.parent_id.clone());
    }
    None
}

fn find_members_by_name(
    ctx: &ResolutionContext<'_>,
    parent_id: &SymbolId,
    name: &str,
) -> Vec<SymbolId> {
    let mut candidates = ctx
        .symbols
        .by_parent
        .get(parent_id)
        .map(|symbols| symbols.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|id| {
            ctx.symbols
                .get(id)
                .map(|symbol| symbol.name == name)
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    normalize_symbol_ids(&mut candidates);
    candidates
}

fn normalize_symbol_ids(ids: &mut Vec<SymbolId>) {
    ids.sort_by(|left, right| left.0.cmp(&right.0));
    ids.dedup();
}

fn call_file_range(call: &CallSite, ctx: &ResolutionContext<'_>) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: call.range.start_line,
            end: call.range.end_line,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ResolutionContext;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        Binding, BindingId, CallSiteId, FileId, Language, ReceiverKind, Scope, ScopeId, ScopeKind,
        SourceRange, Symbol, SymbolKind, Visibility,
    };

    fn symbol(id: &str, name: &str, file: &str, scope_id: Option<&str>) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{name}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: scope_id.map(ScopeId::new),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn with_context<T>(symbols: Vec<Symbol>, test: impl FnOnce(&ResolutionContext<'_>) -> T) -> T {
        with_context_and_bindings(symbols, Vec::new(), test)
    }

    fn with_context_and_bindings<T>(
        symbols: Vec<Symbol>,
        bindings: Vec<Binding>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
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
                end_line: 50,
                end_column: 1,
            },
        }]);
        let symbol_index = SymbolIndex::build(symbols);
        let bindings = BindingIndex::build(bindings);
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

    fn bare_call() -> CallSite {
        CallSite {
            id: CallSiteId::new("call:run"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: ScopeId::new("scope:file"),
            caller_symbol_id: None,
            callee_name: "run".into(),
            receiver: None,
            receiver_kind: ReceiverKind::None,
            range: SourceRange {
                start_line: 20,
                start_column: 5,
                end_line: 20,
                end_column: 10,
            },
        }
    }

    #[test]
    fn lexical_scope_candidate_is_proven_with_call_site_and_scope_proofs() {
        with_context(
            vec![symbol(
                "symbol:run",
                "run",
                "file:src/lib.rs",
                Some("scope:file"),
            )],
            |ctx| {
                let outcome = resolve_bare_call_outcome(&bare_call(), ctx);
                match outcome {
                    ResolutionOutcome::Proven { candidate } => {
                        assert_eq!(candidate.target_symbol_id.0, "symbol:run");
                        assert!(candidate
                            .proofs
                            .iter()
                            .any(|proof| proof.kind == RelationshipProofKind::ExactCallSite));
                        assert!(candidate.proofs.iter().any(|proof| {
                            proof.kind == RelationshipProofKind::SameScopeDefinition
                        }));
                    }
                    other => panic!("expected proven lexical call, got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn lexical_value_binding_shadows_same_named_function() {
        with_context_and_bindings(
            vec![symbol(
                "symbol:run",
                "run",
                "file:src/lib.rs",
                Some("scope:file"),
            )],
            vec![Binding {
                id: BindingId::new("binding:run"),
                file_id: FileId::new("file:src/lib.rs"),
                scope_id: ScopeId::new("scope:file"),
                name: "run".into(),
                declared_type: None,
                inferred_type: None,
                range: SourceRange {
                    start_line: 10,
                    start_column: 5,
                    end_line: 10,
                    end_column: 8,
                },
            }],
            |ctx| match resolve_bare_call_outcome(&bare_call(), ctx) {
                ResolutionOutcome::Unresolved { candidates, .. } => assert!(candidates.is_empty()),
                other => panic!("shadowed bare call must fail closed, got {other:?}"),
            },
        );
    }

    #[test]
    fn same_file_name_only_candidate_is_not_structural_truth() {
        with_context(
            vec![symbol("symbol:run", "run", "file:src/lib.rs", None)],
            |ctx| {
                let outcome = resolve_bare_call_outcome(&bare_call(), ctx);
                match outcome {
                    ResolutionOutcome::Unresolved { candidates, .. } => {
                        assert_eq!(candidates.len(), 1);
                        assert_eq!(candidates[0].target_symbol_id.0, "symbol:run");
                    }
                    other => panic!("expected unresolved heuristic candidate, got {other:?}"),
                }
            },
        );
    }
}
