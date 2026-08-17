use crate::context::ResolutionContext;
use crate::pipeline::{evaluate_candidates, ResolutionOutcome};
use open_kioku_core::{CallSite, GraphEdgeType, SymbolId};
use std::collections::BTreeSet;

pub(crate) fn resolve_self_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(parent_type_id) = caller_parent_type(call, ctx) else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };

    if let Some(receiver_member) = self_receiver_member(call) {
        let mut type_names = BTreeSet::new();
        for field_id in crate::typed_calls::find_members_by_name(ctx, &parent_type_id, receiver_member)
        {
            if let Some(signature) = ctx
                .symbols
                .get(&field_id)
                .and_then(|symbol| symbol.signature.as_deref())
            {
                let signature = signature.trim();
                if !signature.is_empty() {
                    type_names.insert(signature.to_string());
                }
            }
        }
        if let Some(binding) = ctx.bindings.resolve_before(
            &call.scope_id,
            receiver_member,
            &call.range,
            ctx.scopes,
        ) {
            if let Some(type_name) = binding
                .declared_type
                .as_deref()
                .or(binding.inferred_type.as_deref())
            {
                let type_name = type_name.trim();
                if !type_name.is_empty() {
                    type_names.insert(type_name.to_string());
                }
            }
        }
        if !type_names.is_empty() {
            return crate::typed_calls::resolve_type_names_member_outcome(
                call,
                ctx,
                &type_names.into_iter().collect::<Vec<_>>(),
            );
        }
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    }

    let direct = crate::typed_calls::find_members_by_name(ctx, &parent_type_id, &call.callee_name);
    if !direct.is_empty() {
        return crate::typed_calls::evaluate_direct_member_targets(call, ctx, direct);
    }

    let inherited = ctx.inheritance.inherited_member_candidates(
        &parent_type_id,
        &call.callee_name,
        ctx.symbols,
    );
    crate::typed_calls::evaluate_inherited_targets(call, ctx, inherited)
}

pub(crate) fn resolve_super_member_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(parent_type_id) = caller_parent_type(call, ctx) else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    let inherited = ctx.inheritance.inherited_member_candidates(
        &parent_type_id,
        &call.callee_name,
        ctx.symbols,
    );
    crate::typed_calls::evaluate_inherited_targets(call, ctx, inherited)
}

fn caller_parent_type(call: &CallSite, ctx: &ResolutionContext<'_>) -> Option<SymbolId> {
    call.caller_symbol_id
        .as_ref()
        .and_then(|caller_id| ctx.symbols.get(caller_id))
        .and_then(|caller| caller.parent_symbol_id.clone())
}

fn self_receiver_member(call: &CallSite) -> Option<&str> {
    let receiver = call.receiver.as_deref()?;
    let stripped = receiver
        .trim_start_matches("this.")
        .trim_start_matches("self.")
        .trim_start_matches("Self::");
    if stripped.is_empty()
        || matches!(stripped, "this" | "self" | "Self")
        || stripped == receiver
    {
        None
    } else {
        Some(stripped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ResolutionContext;
    use crate::index::{BindingIndex, ScopeIndex, SymbolIndex};
    use crate::inheritance::InheritanceIndex;
    use open_kioku_core::{
        CallSiteId, Confidence, EvidenceSourceType, FileId, Language, ReceiverKind, Scope,
        ScopeId, ScopeKind, SourceRange, Symbol, SymbolKind, Visibility,
    };

    fn symbol(
        id: &str,
        name: &str,
        kind: SymbolKind,
        parent: Option<&str>,
    ) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind,
            file_id: FileId::new("file:src/lib.rs"),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: parent.map(SymbolId::new),
            scope_id: Some(ScopeId::new("scope:file")),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn self_call(kind: ReceiverKind) -> CallSite {
        CallSite {
            id: CallSiteId::new("call:self.run"),
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: ScopeId::new("scope:file"),
            caller_symbol_id: Some(SymbolId::new("symbol:caller")),
            callee_name: "run".into(),
            receiver: Some(match kind {
                ReceiverKind::Super => "super".into(),
                _ => "self".into(),
            }),
            receiver_kind: kind,
            range: SourceRange {
                start_line: 20,
                start_column: 5,
                end_line: 20,
                end_column: 14,
            },
        }
    }

    fn with_context<T>(test: impl FnOnce(&ResolutionContext<'_>) -> T) -> T {
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
        let symbols = SymbolIndex::build(vec![
            symbol("symbol:type:Service", "Service", SymbolKind::Class, None),
            symbol(
                "symbol:caller",
                "caller",
                SymbolKind::Method,
                Some("symbol:type:Service"),
            ),
            symbol(
                "symbol:method:run",
                "run",
                SymbolKind::Method,
                Some("symbol:type:Service"),
            ),
        ]);
        let bindings = BindingIndex::build(Vec::new());
        let inheritance = InheritanceIndex::build(Vec::new());
        let repository = open_kioku_semantic_model::SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let context = ResolutionContext::new(
            &file_id,
            std::path::Path::new("src/lib.rs"),
            None,
            Language::Rust,
            &repository,
            &symbols,
            &scopes,
            &bindings,
            &inheritance,
            semantics,
        );
        test(&context)
    }

    #[test]
    fn direct_self_member_is_proven_from_containing_type() {
        with_context(|ctx| match resolve_self_member_outcome(&self_call(ReceiverKind::Self_), ctx) {
            ResolutionOutcome::Proven { candidate } => {
                assert_eq!(candidate.target_symbol_id.0, "symbol:method:run");
            }
            other => panic!("expected proven direct self call, got {other:?}"),
        });
    }
}
