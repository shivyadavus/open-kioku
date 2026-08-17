use crate::context::ResolutionContext;
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, Language, LineRange,
    RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
};
use std::collections::BTreeMap;

pub(crate) fn resolve_typed_receiver_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let lookup_name = receiver
        .trim_start_matches("this.")
        .trim_start_matches("self.")
        .trim_start_matches("Self::");

    let Some(binding) =
        ctx.bindings
            .resolve_before(&call.scope_id, lookup_name, &call.range, ctx.scopes)
    else {
        return imported_receiver_outcome(call, ctx, lookup_name);
    };

    let Some(type_name) = binding
        .declared_type
        .as_deref()
        .or(binding.inferred_type.as_deref())
    else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };

    resolve_named_type_member_outcome(call, ctx, type_name)
}

pub(crate) fn resolve_static_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let typed = resolve_named_type_member_outcome(call, ctx, receiver);
    match typed {
        ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty() => {
            imported_receiver_outcome(call, ctx, receiver)
        }
        other => other,
    }
}

pub(crate) fn resolve_module_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    if ctx.language == Language::Rust {
        if let Some(outcome) = resolve_rust_qualified_module_outcome(call, ctx, receiver) {
            return outcome;
        }
    }
    let imported = imported_receiver_outcome(call, ctx, receiver);
    match imported {
        ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty() => {
            resolve_named_type_member_outcome(call, ctx, receiver)
        }
        other => other,
    }
}

fn resolve_rust_qualified_module_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> Option<ResolutionOutcome> {
    let module = receiver.strip_prefix("crate::")?;
    if module.is_empty() {
        return None;
    }

    let mut targets = Vec::new();
    for qualified_name in rust_qualified_module_symbol_names(module, &call.callee_name) {
        if let Some(ids) = ctx.symbols.by_qualified.get(&qualified_name) {
            targets.extend(ids.iter().cloned());
        }
    }
    normalize_symbol_ids(&mut targets);
    if targets.is_empty() {
        return None;
    }

    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::LexicalScope,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "candidate from exact Rust crate-qualified module path".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ModuleOrPackageBinding,
                "rust_crate_qualified_module",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "rust_crate_qualified_member",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    Some(evaluate_candidates(&GraphEdgeType::Calls, candidates))
}

fn rust_qualified_module_symbol_names(module: &str, callee: &str) -> [String; 2] {
    [
        format!("src::{module}::{callee}"),
        format!("src::{module}::mod::{callee}"),
    ]
}

pub(crate) fn resolve_named_type_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_name: &str,
) -> ResolutionOutcome {
    resolve_type_names_member_outcome(call, ctx, &[type_name.to_string()])
}

pub(crate) fn resolve_type_names_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_names: &[String],
) -> ResolutionOutcome {
    let mut type_candidates = Vec::new();
    for type_name in type_names {
        type_candidates.extend(collect_type_candidates(ctx, &call.scope_id, type_name));
    }
    normalize_symbol_ids(&mut type_candidates);
    if type_candidates.is_empty() {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    let mut direct_targets = Vec::new();
    for type_id in &type_candidates {
        direct_targets.extend(find_members_by_name(ctx, type_id, &call.callee_name));
    }
    normalize_symbol_ids(&mut direct_targets);
    if !direct_targets.is_empty() {
        return evaluate_direct_member_targets(call, ctx, direct_targets);
    }

    let mut inherited_targets = Vec::new();
    for type_id in &type_candidates {
        inherited_targets.extend(ctx.inheritance.inherited_member_candidates(
            type_id,
            &call.callee_name,
            ctx.symbols,
        ));
    }
    normalize_symbol_ids(&mut inherited_targets);
    evaluate_inherited_targets(call, ctx, inherited_targets)
}

pub(crate) fn imported_receiver_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> ResolutionOutcome {
    let mut targets = Vec::new();
    let import_bindings =
        ctx.repository
            .imports
            .lookup(ctx.file_id, Some(&call.scope_id), receiver);

    for binding in import_bindings {
        if let Some(module_id) = &binding.resolved_module {
            for export in ctx.repository.exports.lookup(module_id, &call.callee_name) {
                if let Some(target) = &export.origin_symbol {
                    targets.push(target.clone());
                }
            }
        }
        if let Some(target_file) = &binding.target_file {
            for ((_, exported_name), exports) in &ctx.repository.exports.by_module_exported_name {
                if exported_name != &call.callee_name {
                    continue;
                }
                for export in exports {
                    if &export.file_id == target_file {
                        if let Some(target) = &export.origin_symbol {
                            targets.push(target.clone());
                        }
                    }
                }
            }
            if let Some(file_symbols) = ctx.symbols.by_file.get(target_file) {
                for id in file_symbols {
                    if ctx
                        .symbols
                        .get(id)
                        .map(|symbol| {
                            symbol.name == call.callee_name && symbol.parent_symbol_id.is_none()
                        })
                        .unwrap_or(false)
                    {
                        targets.push(id.clone());
                    }
                }
            }
        }
    }
    normalize_symbol_ids(&mut targets);
    if targets.is_empty() {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::ExplicitImport,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "receiver call candidate from exact import/module binding".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ImportBinding,
                "receiver_import_binding",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "receiver_import_member",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

pub(crate) fn evaluate_direct_member_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
) -> ResolutionOutcome {
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "method candidate from typed receiver binding".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ReceiverType,
                "typed_receiver",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::ContainingType,
                "direct_member_of_receiver_type",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

pub(crate) fn evaluate_inherited_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
) -> ResolutionOutcome {
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::InheritanceGraph,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "inherited receiver candidate retained without authoritative uniqueness"
                    .into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::InheritanceBinding,
                "receiver_inheritance_candidate",
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate
        })
        .collect();
    evaluate_candidates(&GraphEdgeType::Calls, candidates)
}

pub(crate) fn collect_type_candidates(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    type_name: &str,
) -> Vec<SymbolId> {
    let mut candidates = BTreeMap::<String, SymbolId>::new();

    if let Some(file_symbols) = ctx.symbols.by_file.get(ctx.file_id) {
        for id in file_symbols {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == type_name)
                .unwrap_or(false)
            {
                candidates.insert(id.0.clone(), id.clone());
            }
        }
    }

    for binding in ctx
        .repository
        .imports
        .lookup(ctx.file_id, Some(scope_id), type_name)
    {
        if let Some(target) = &binding.target_symbol {
            if ctx
                .symbols
                .get(target)
                .map(|symbol| is_type_symbol(&symbol.kind))
                .unwrap_or(false)
            {
                candidates.insert(target.0.clone(), target.clone());
            }
        }
        if let Some(target_file) = &binding.target_file {
            if let Some(file_symbols) = ctx.symbols.by_file.get(target_file) {
                for id in file_symbols {
                    if ctx
                        .symbols
                        .get(id)
                        .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == type_name)
                        .unwrap_or(false)
                    {
                        candidates.insert(id.0.clone(), id.clone());
                    }
                }
            }
        }
    }

    if let Some(qualified) = ctx.symbols.by_qualified.get(type_name) {
        for id in qualified {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind))
                .unwrap_or(false)
            {
                candidates.insert(id.0.clone(), id.clone());
            }
        }
    }

    candidates.into_values().collect()
}

fn is_type_symbol(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
}

pub(crate) fn find_members_by_name(
    ctx: &ResolutionContext<'_>,
    parent_id: &SymbolId,
    name: &str,
) -> Vec<SymbolId> {
    let mut candidates = ctx
        .symbols
        .by_parent
        .get(parent_id)
        .map(Vec::as_slice)
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

fn ambiguity_strings(ids: &[SymbolId]) -> Vec<String> {
    if ids.len() > 1 {
        ids.iter().map(|id| id.0.clone()).collect()
    } else {
        Vec::new()
    }
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
fn proof(
    kind: RelationshipProofKind,
    strategy: &str,
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    target: &SymbolId,
    candidate_count: usize,
    ambiguity: &[String],
) -> RelationshipProof {
    let mut proof = RelationshipProof::new(kind, strategy, candidate_count);
    proof.source_range = call_file_range(call, ctx);
    proof.source_symbol_id = call.caller_symbol_id.clone();
    proof.target_symbol_id = Some(target.clone());
    proof.ambiguity = ambiguity.to_vec();
    proof
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
        Binding, BindingId, CallSiteId, FileId, Language, ReceiverKind, Scope, ScopeKind,
        SourceRange, Symbol, Visibility,
    };

    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        assert_eq!(
            rust_qualified_module_symbol_names("storage", "persist"),
            [
                "src::storage::persist".to_string(),
                "src::storage::mod::persist".to_string(),
            ]
        );
    }

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
            vec![
                "symbol:method:a.run".to_string(),
                "symbol:method:b.run".to_string()
            ]
        );
        assert_eq!(
            extract(second),
            vec![
                "symbol:method:a.run".to_string(),
                "symbol:method:b.run".to_string()
            ]
        );
    }
}
