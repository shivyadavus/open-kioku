use crate::context::{ResolutionContext, RustRelativeModule, ScopedImport};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    Binding, CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, Language,
    LineRange, RelationshipProof, RelationshipProofKind, ScopeId, SymbolId, SymbolKind,
};
use std::collections::BTreeMap;

pub(crate) fn resolve_typed_receiver_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> ResolutionOutcome {
    let Some(receiver) = call.receiver.as_deref() else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };
    // `self.field` is looked up by a local binding of the field's name, which is not the field.
    let through_self_field = receiver.starts_with("self.");
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

    let Some((type_name, proven)) = binding_receiver_type(ctx, &call.scope_id, binding) else {
        return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
    };

    resolve_type_names_member_outcome_with(call, ctx, &[type_name], proven && !through_self_field)
}

/// The receiver type a binding gives, and whether the index proves it. A written type does; an
/// inferred one does when `inferred_receiver_type` can prove it.
pub(crate) fn binding_receiver_type(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    binding: &Binding,
) -> Option<(String, bool)> {
    if let Some(declared) = binding
        .declared_type
        .as_deref()
        .map(str::trim)
        .filter(|declared| !declared.is_empty())
    {
        return Some((declared.to_string(), true));
    }
    let inferred = binding
        .inferred_type
        .as_deref()
        .map(str::trim)
        .filter(|inferred| !inferred.is_empty())?;
    Some(inferred_receiver_type(ctx, scope_id, inferred))
}

/// The type an initializer gives its binding, and whether the index proves it.
///
/// A constructor form the parser recognizes (`Foo { .. }`, `new Foo()`, the `self` parameter) is
/// proof. A Rust path call, recorded as `Foo::bar()`, is proof of `Foo` only when every `bar` on
/// every `Foo` candidate returns `Self` or `Foo` by its indexed signature: `Server::spawn()`
/// returning a `ServerHandle` must not type its binding as `Server`.
fn inferred_receiver_type(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    inferred: &str,
) -> (String, bool) {
    let Some(call_path) = inferred.strip_suffix("()") else {
        return (inferred.to_string(), true);
    };
    let Some((owner, constructor)) = call_path.rsplit_once("::") else {
        return (call_path.to_string(), false);
    };
    let owner_name = owner.rsplit("::").next().unwrap_or(owner);
    let owner_types = collect_type_candidates(ctx, scope_id, owner);
    let constructors = owner_types
        .iter()
        .flat_map(|type_id| find_members_by_name(ctx, type_id, constructor))
        .collect::<Vec<_>>();
    // `#[derive(Default)]` adds no indexed member, but `Default::default` returns `Self` by the
    // trait's definition. It proves a struct, enum or alias; a module path such as
    // `config::default()` is a function call, not a constructor.
    if constructors.is_empty() && constructor == "default" {
        let proven = !owner_types.is_empty()
            && owner_types.iter().all(|type_id| {
                ctx.symbols
                    .get(type_id)
                    .is_some_and(|symbol| symbol.kind == SymbolKind::Class)
            });
        return (owner.to_string(), proven);
    }
    let proven = !constructors.is_empty()
        && constructors.iter().all(|id| {
            ctx.symbols
                .get(id)
                .and_then(|symbol| symbol.signature.as_deref())
                .and_then(rust_signature_return_type)
                .is_some_and(|returns| returns == "Self" || returns == owner_name)
        });
    (owner.to_string(), proven)
}

/// The return type in a Rust function signature as the parser records it, `fn(<params>) <type>`.
fn rust_signature_return_type(signature: &str) -> Option<&str> {
    let after_fn = signature.strip_prefix("fn")?;
    let mut depth = 0usize;
    for (index, ch) in after_fn.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let returns = after_fn[index + 1..].trim();
                    return (!returns.is_empty()).then_some(returns);
                }
            }
            _ => {}
        }
    }
    None
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

/// A Rust call through a `crate::`, `self::` or `super::` path. `self` and `super` start from the
/// innermost module around the call, an inline `mod` block included: `super::f()` in
/// `mod tests` of `src/worker.rs` names the `f` that file declares, not one in the crate root. A
/// path ending in a module of this file names an item that module declares or brings in from
/// another module of the file; one ending outside the file's scopes names items by the qualified
/// names its module path spells, unless the declared module tree shows the file holding them, or
/// this file, is not the module its path spells.
fn resolve_rust_qualified_module_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> Option<ResolutionOutcome> {
    let receiver = receiver.trim();
    let (mut targets, strategy) = if receiver == "crate" || receiver.starts_with("crate::") {
        let names = rust_crate_path_member_names(ctx.file_path, receiver, &call.callee_name)?;
        (
            rust_qualified_targets(ctx, &names),
            RustModulePathStrategy::CrateQualified,
        )
    } else {
        let (depth, segments) = rust_relative_path(receiver)?;
        match crate::context::rust_relative_module(ctx, &call.scope_id, depth, &segments)? {
            RustRelativeModule::InFile(module) => (
                crate::context::rust_module_items(ctx, module, &call.callee_name, |symbol| {
                    !matches!(symbol.kind, SymbolKind::Module | SymbolKind::Package)
                }),
                RustModulePathStrategy::ModuleScope,
            ),
            RustRelativeModule::Outside { climbs, path } => {
                // The path is read off this file's own path, which a `#[path]` mount makes wrong.
                if !ctx.scopes.may_be_module_at_its_path(ctx.file_id) {
                    return None;
                }
                let names =
                    rust_outside_member_names(ctx.file_path, climbs, &path, &call.callee_name)?;
                (
                    rust_qualified_targets(ctx, &names),
                    RustModulePathStrategy::CrateQualified,
                )
            }
        }
    };
    if matches!(strategy, RustModulePathStrategy::CrateQualified) {
        // A file the module tree does not declare where its path says, such as the default
        // location of a `#[path]` module, is not the module its qualified name spells.
        targets.retain(|target| {
            ctx.symbols
                .get(target)
                .is_some_and(|symbol| ctx.scopes.may_be_module_at_its_path(&symbol.file_id))
        });
    }
    normalize_symbol_ids(&mut targets);
    if targets.is_empty() {
        return None;
    }

    let (message, module_strategy, member_strategy) = match strategy {
        RustModulePathStrategy::CrateQualified => (
            "candidate from exact Rust crate-qualified module path",
            "rust_crate_qualified_module",
            "rust_crate_qualified_member",
        ),
        RustModulePathStrategy::ModuleScope => (
            "candidate declared by the module of this file that the Rust path names",
            "rust_module_scope_path",
            "rust_module_scope_member",
        ),
    };
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
                message: message.into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ModuleOrPackageBinding,
                module_strategy,
                call,
                ctx,
                &target,
                candidate_count,
                &ambiguity,
            ));
            candidate.proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                member_strategy,
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

/// How a Rust module path reached its candidates.
enum RustModulePathStrategy {
    /// Qualified names the path spells from the file path.
    CrateQualified,
    /// Items a module scope of this file declares.
    ModuleScope,
}

fn rust_qualified_targets(ctx: &ResolutionContext<'_>, names: &[String]) -> Vec<SymbolId> {
    names
        .iter()
        .filter_map(|name| ctx.symbols.by_qualified.get(name))
        .flat_map(|ids| ids.iter().cloned())
        .collect()
}

/// `self::a::b` is `(0, ["a", "b"])` and `super::super::a` is `(2, ["a"])`; a path that does not
/// start with `self` or `super`, or names either again later, is `None`.
fn rust_relative_path(receiver: &str) -> Option<(usize, Vec<&str>)> {
    let mut segments = receiver.split("::").map(str::trim);
    let depth = match segments.next()? {
        "self" => 0,
        "super" => 1,
        _ => return None,
    };
    let mut depth = depth;
    let mut rest = Vec::new();
    for segment in segments {
        match segment {
            "super" if depth > 0 && rest.is_empty() => depth += 1,
            "" | "self" | "super" | "crate" => return None,
            segment => rest.push(segment),
        }
    }
    Some((depth, rest))
}

fn rust_crate_path_member_names(
    file_path: &std::path::Path,
    receiver: &str,
    callee: &str,
) -> Option<Vec<String>> {
    let stem = rust_file_stem(file_path);
    let mut names = if receiver == "crate" {
        rust_crate_root_member_names(&stem, callee)
    } else {
        let module = receiver.strip_prefix("crate::")?;
        rust_module_member_names(&format!("src::{module}"), callee)
    };
    names.sort();
    names.dedup();
    Some(names)
}

/// Qualified names of `callee` in the module `climbs` modules above this file's own module and
/// then down `path`, as tree-sitter spells them from module file paths.
fn rust_outside_member_names(
    file_path: &std::path::Path,
    climbs: usize,
    path: &[String],
    callee: &str,
) -> Option<Vec<String>> {
    let mut module = rust_logical_module_prefix(&rust_file_stem(file_path));
    for _ in 0..climbs {
        module = rust_parent_module_prefix(&module)?;
    }
    let mut names = if path.is_empty() {
        if module.contains("::") {
            rust_module_member_names(&module, callee)
        } else {
            vec![
                format!("{module}::lib::{callee}"),
                format!("{module}::main::{callee}"),
            ]
        }
    } else {
        rust_module_member_names(&format!("{module}::{}", path.join("::")), callee)
    };
    names.sort();
    names.dedup();
    Some(names)
}

fn rust_file_stem(file_path: &std::path::Path) -> String {
    file_path
        .with_extension("")
        .to_string_lossy()
        .replace(['/', '\\'], "::")
}

fn rust_logical_module_prefix(stem: &str) -> String {
    if let Some(prefix) = stem.strip_suffix("::mod") {
        prefix.to_string()
    } else if let Some(prefix) = stem.strip_suffix("::lib") {
        prefix.to_string()
    } else if let Some(prefix) = stem.strip_suffix("::main") {
        prefix.to_string()
    } else {
        stem.to_string()
    }
}

fn rust_parent_module_prefix(module: &str) -> Option<String> {
    module
        .rsplit_once("::")
        .map(|(parent, _)| parent.to_string())
}

fn rust_crate_root_member_names(stem: &str, callee: &str) -> Vec<String> {
    if stem.ends_with("::lib") || stem.ends_with("::main") {
        vec![format!("{stem}::{callee}")]
    } else {
        let root = stem.split("::").next().unwrap_or("src");
        vec![
            format!("{root}::lib::{callee}"),
            format!("{root}::main::{callee}"),
        ]
    }
}

fn rust_module_member_names(module: &str, callee: &str) -> Vec<String> {
    vec![
        format!("{module}::{callee}"),
        format!("{module}::mod::{callee}"),
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
    resolve_type_names_member_outcome_with(call, ctx, type_names, true)
}

/// Member calls on a receiver of one of `type_names`. `receiver_type_proven` is false when the
/// receiver's type is a candidate the index cannot prove; its members are then reported without
/// the receiver-type proof, so they cannot become structural truth.
pub(crate) fn resolve_type_names_member_outcome_with(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    type_names: &[String],
    receiver_type_proven: bool,
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
        return if receiver_type_proven {
            evaluate_direct_member_targets(call, ctx, direct_targets)
        } else {
            evaluate_unproven_member_targets(call, ctx, direct_targets)
        };
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
    let import_bindings = match ctx.scoped_import(&call.scope_id, receiver, |binding| {
        binding.target_file.is_some() || binding.resolved_module.is_some()
    }) {
        ScopedImport::Resolved(bindings) => bindings,
        ScopedImport::NotImported | ScopedImport::Unresolved => Vec::new(),
    };

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

/// Members of a receiver type the index does not prove. They are kept as candidates with the
/// containing-type proof only; without the receiver-type proof they stay below authoritative.
fn evaluate_unproven_member_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
) -> ResolutionOutcome {
    let candidate_count = targets.len();
    let ambiguity = ambiguity_strings(&targets);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::High);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::TypedBinding,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: "method candidate from a receiver type the index does not prove".into(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            candidate.proofs.push(proof(
                RelationshipProofKind::ContainingType,
                "direct_member_of_unproven_receiver_type",
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
    collect_type_candidate_origins(ctx, scope_id, type_name)
        .into_iter()
        .map(|(target, _)| target)
        .collect()
}

/// Type candidates for `type_name` at `scope_id`, each with whether an import binding reached it.
pub(crate) fn collect_type_candidate_origins(
    ctx: &ResolutionContext<'_>,
    scope_id: &ScopeId,
    type_name: &str,
) -> Vec<(SymbolId, bool)> {
    let mut candidates = BTreeMap::<String, (SymbolId, bool)>::new();
    let mut add = |target: &SymbolId, via_import: bool| {
        candidates
            .entry(target.0.clone())
            .or_insert_with(|| (target.clone(), false))
            .1 |= via_import;
    };

    if ctx.language == Language::Rust {
        // A Rust type of this file is nameable only from its own module, or through an import
        // the scoped lookup below reads. A type in a nearer scope shadows every import further
        // out, and one in a sibling `mod` block is never a candidate.
        if let Some(types) =
            crate::context::nearest_lexical_items(ctx, scope_id, type_name, |symbol| {
                is_type_symbol(&symbol.kind)
            })
        {
            return types.into_iter().map(|target| (target, false)).collect();
        }
    } else if let Some(file_symbols) = ctx.symbols.by_file.get(ctx.file_id) {
        for id in file_symbols {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| is_type_symbol(&symbol.kind) && symbol.name == type_name)
                .unwrap_or(false)
            {
                add(id, false);
            }
        }
    }

    // As for bare calls, the nearest import of the name in scope decides.
    if let ScopedImport::Resolved(bindings) = ctx.scoped_import(scope_id, type_name, |binding| {
        binding.target_symbol.is_some() || binding.target_file.is_some()
    }) {
        for binding in bindings {
            if let Some(target) = &binding.target_symbol {
                if ctx
                    .symbols
                    .get(target)
                    .map(|symbol| is_type_symbol(&symbol.kind))
                    .unwrap_or(false)
                {
                    add(target, true);
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
                            add(id, true);
                        }
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
                add(id, false);
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
        path: ctx.file_path.into(),
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
        Binding, BindingId, CallSiteId, FileId, Language, ModuleDeclarationSite, ReceiverKind,
        Scope, ScopeKind, SourceRange, Symbol, Visibility,
    };

    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        assert_eq!(
            rust_crate_path_member_names(
                std::path::Path::new("src/lib.rs"),
                "crate::storage",
                "persist"
            )
            .unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_outside_member_names(
                std::path::Path::new("src/storage/service.rs"),
                1,
                &[],
                "persist"
            )
            .unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_outside_member_names(std::path::Path::new("src/worker.rs"), 1, &[], "persist")
                .unwrap(),
            vec![
                "src::lib::persist".to_string(),
                "src::main::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_outside_member_names(
                std::path::Path::new("src/worker.rs"),
                0,
                &["tests".to_string(), "helpers".to_string()],
                "persist"
            )
            .unwrap(),
            vec![
                "src::worker::tests::helpers::mod::persist".to_string(),
                "src::worker::tests::helpers::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_outside_member_names(std::path::Path::new("src/lib.rs"), 1, &[], "persist"),
            None
        );
    }

    #[test]
    fn rust_relative_paths_count_leading_super_segments() {
        assert_eq!(rust_relative_path("self"), Some((0, vec![])));
        assert_eq!(rust_relative_path("self::a::b"), Some((0, vec!["a", "b"])));
        assert_eq!(rust_relative_path("super"), Some((1, vec![])));
        assert_eq!(rust_relative_path("super::super::a"), Some((2, vec!["a"])));
        assert_eq!(rust_relative_path("self::super"), None);
        assert_eq!(rust_relative_path("super::a::super"), None);
        assert_eq!(rust_relative_path("crate::a"), None);
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

    fn with_receiver_binding<T>(
        symbols: Vec<Symbol>,
        inferred_type: &str,
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
                end_line: 100,
                end_column: 1,
            },
        }]);
        let bindings = BindingIndex::build(vec![Binding {
            id: BindingId::new("binding:svc"),
            file_id: file_id.clone(),
            scope_id: ScopeId::new("scope:file"),
            name: "svc".into(),
            declared_type: None,
            inferred_type: Some(inferred_type.into()),
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
    fn inferred_path_call_receiver_is_proven_only_by_the_constructor_signature() {
        // `let svc = Service::open(..);` then `svc.run()`.
        let symbols = |signature: &str| {
            vec![
                type_symbol("symbol:type:Service", "Service"),
                method_symbol("symbol:method:Service.run", "symbol:type:Service"),
                Symbol {
                    name: "open".into(),
                    signature: Some(signature.into()),
                    ..method_symbol("symbol:method:Service.open", "symbol:type:Service")
                },
            ]
        };
        for (signature, proven) in [
            ("fn() Self", true),
            ("fn(port: u16) Service", true),
            ("fn() ServiceHandle", false),
            ("fn() Option<Self>", false),
        ] {
            with_receiver_binding(symbols(signature), "Service::open()", |ctx| {
                match resolve_typed_receiver_outcome(&call(), ctx) {
                    ResolutionOutcome::Proven { candidate } => {
                        assert!(proven, "`{signature}` must not prove the receiver type");
                        assert_eq!(candidate.target_symbol_id.0, "symbol:method:Service.run");
                    }
                    ResolutionOutcome::Unresolved { candidates, .. } => {
                        assert!(!proven, "`{signature}` proves the receiver type");
                        assert_eq!(candidates.len(), 1, "the member stays a candidate");
                    }
                    other => panic!("`{signature}`: unexpected outcome {other:?}"),
                }
            });
        }
    }

    #[test]
    fn self_field_receiver_matched_through_a_local_binding_is_not_proven() {
        // `self.svc.run()` where only a local `svc: Service` binding exists.
        with_context(
            vec![
                type_symbol("symbol:type:Service", "Service"),
                method_symbol("symbol:method:Service.run", "symbol:type:Service"),
            ],
            |ctx| {
                let call = CallSite {
                    receiver: Some("self.svc".into()),
                    ..call()
                };
                match resolve_typed_receiver_outcome(&call, ctx) {
                    ResolutionOutcome::Unresolved { candidates, .. } => {
                        assert_eq!(candidates.len(), 1)
                    }
                    other => panic!("expected an unproven candidate, got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn derived_default_proves_a_struct_receiver_but_not_a_module_path() {
        // `let svc = Service::default();` with `#[derive(Default)]`: no indexed `default` member.
        with_receiver_binding(
            vec![
                type_symbol("symbol:type:Service", "Service"),
                method_symbol("symbol:method:Service.run", "symbol:type:Service"),
            ],
            "Service::default()",
            |ctx| match resolve_typed_receiver_outcome(&call(), ctx) {
                ResolutionOutcome::Proven { candidate } => {
                    assert_eq!(candidate.target_symbol_id.0, "symbol:method:Service.run")
                }
                other => panic!("expected a proven call on a derived default, got {other:?}"),
            },
        );

        // `let svc = config::default();` names a module function, not a constructor.
        with_receiver_binding(
            vec![
                Symbol {
                    kind: SymbolKind::Module,
                    ..type_symbol("symbol:module:config", "config")
                },
                method_symbol("symbol:method:config.run", "symbol:module:config"),
            ],
            "config::default()",
            |ctx| match resolve_typed_receiver_outcome(&call(), ctx) {
                ResolutionOutcome::Unresolved { candidates, .. } => assert_eq!(candidates.len(), 1),
                other => panic!("expected an unproven candidate, got {other:?}"),
            },
        );
    }

    /// `src/worker.rs` declaring `fn f`, `mod child;` and
    /// `mod outer { fn f; mod helpers; mod inner { fn g; fn t() { .. } } }` beside a crate root
    /// `src/lib.rs` that declares its own `fn f`. Qualified names are the file's, as tree-sitter
    /// spells them, so every `f` of the worker shares one. As the parser does, each bodiless
    /// `mod name;` has a scope of its own; `declarations` says whether the index also holds the
    /// module declarations that tell such a scope from a block.
    fn with_inline_mod_context<T>(
        extra: Vec<Symbol>,
        declarations: bool,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        with_misplaced_module_files(extra, declarations, &[], test)
    }

    /// [`with_inline_mod_context`], with `misplaced` recorded as the files the declared module
    /// tree shows are not the module their path spells.
    fn with_misplaced_module_files<T>(
        extra: Vec<Symbol>,
        declarations: bool,
        misplaced: &[&str],
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        let worker = FileId::new("file:src/worker.rs");
        let range = |line: u32| SourceRange {
            start_line: line,
            start_column: 1,
            end_line: line + 1,
            end_column: 1,
        };
        let scope = |id: &str, parent: Option<&str>, owner: Option<&str>, kind, line| Scope {
            id: ScopeId::new(id),
            file_id: worker.clone(),
            parent_id: parent.map(ScopeId::new),
            owner_symbol_id: owner.map(SymbolId::new),
            kind,
            range: range(line),
        };
        let module = |id: &str, parent: &str, owner: &str, line| {
            scope(id, Some(parent), Some(owner), ScopeKind::Module, line)
        };
        let mut scopes = ScopeIndex::build(vec![
            scope("scope:worker", None, None, ScopeKind::File, 1),
            module("scope:child", "scope:worker", "sym:mod:child", 2),
            module("scope:outer", "scope:worker", "sym:mod:outer", 3),
            module("scope:helpers", "scope:outer", "sym:mod:helpers", 4),
            module("scope:inner", "scope:outer", "sym:mod:inner", 5),
            scope(
                "scope:t",
                Some("scope:inner"),
                Some("sym:inner:t"),
                ScopeKind::Function,
                6,
            ),
            scope(
                "scope:t:body",
                Some("scope:t"),
                Some("sym:inner:t"),
                ScopeKind::Block,
                7,
            ),
        ]);
        if declarations {
            let declaration = |parent: &str, name: &str, has_body, line| ModuleDeclarationSite {
                file_id: worker.clone(),
                scope_id: Some(ScopeId::new(parent)),
                name: name.into(),
                has_body,
                has_path_attribute: false,
                range: range(line),
            };
            scopes.record_module_declarations(&[
                declaration("scope:worker", "child", false, 2),
                declaration("scope:worker", "outer", true, 3),
                declaration("scope:outer", "helpers", false, 4),
                declaration("scope:outer", "inner", true, 5),
            ]);
        }
        scopes.record_misplaced_rust_module_files(
            misplaced
                .iter()
                .map(|path| FileId::new(format!("file:{path}")))
                .collect(),
        );
        let item = |id: &str, name: &str, kind: SymbolKind, file: &str, scope: &str| Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("src::{file}::{name}"),
            kind,
            file_id: FileId::new(format!("file:src/{}.rs", file.replace("::", "/"))),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: Some(ScopeId::new(scope)),
            signature: None,
            visibility: Visibility::Public,
        };
        let function = SymbolKind::Function;
        let mut symbols = vec![
            item("sym:lib:f", "f", function.clone(), "lib", "scope:lib"),
            item(
                "sym:worker:f",
                "f",
                function.clone(),
                "worker",
                "scope:worker",
            ),
            item(
                "sym:mod:child",
                "child",
                SymbolKind::Module,
                "worker",
                "scope:worker",
            ),
            item(
                "sym:child:c",
                "c",
                function.clone(),
                "worker::child",
                "scope:c",
            ),
            item(
                "sym:mod:outer",
                "outer",
                SymbolKind::Module,
                "worker",
                "scope:worker",
            ),
            item(
                "sym:outer:f",
                "f",
                function.clone(),
                "worker",
                "scope:outer",
            ),
            item(
                "sym:mod:helpers",
                "helpers",
                SymbolKind::Module,
                "worker",
                "scope:outer",
            ),
            item(
                "sym:mod:inner",
                "inner",
                SymbolKind::Module,
                "worker",
                "scope:outer",
            ),
            item(
                "sym:inner:g",
                "g",
                function.clone(),
                "worker",
                "scope:inner",
            ),
            item("sym:inner:t", "t", function, "worker", "scope:inner"),
        ];
        symbols.extend(extra);
        let symbol_index = SymbolIndex::build(symbols);
        let bindings = BindingIndex::build(Vec::new());
        let inheritance = InheritanceIndex::build(Vec::new());
        let repository = open_kioku_semantic_model::SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let context = ResolutionContext::new(
            &worker,
            std::path::Path::new("src/worker.rs"),
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

    fn module_path_call(scope: &str, receiver: &str, callee: &str) -> CallSite {
        CallSite {
            id: CallSiteId::new(format!("call:{receiver}::{callee}")),
            file_id: FileId::new("file:src/worker.rs"),
            scope_id: ScopeId::new(scope),
            caller_symbol_id: Some(SymbolId::new("sym:inner:t")),
            callee_name: callee.into(),
            receiver: Some(receiver.into()),
            receiver_kind: ReceiverKind::Module,
            range: SourceRange {
                start_line: 20,
                start_column: 5,
                end_line: 20,
                end_column: 14,
            },
        }
    }

    fn proven_target(ctx: &ResolutionContext<'_>, call: &CallSite) -> Option<String> {
        match resolve_module_member_outcome(call, ctx) {
            ResolutionOutcome::Proven { candidate } => Some(candidate.target_symbol_id.0),
            ResolutionOutcome::Unresolved { candidates, .. } if candidates.is_empty() => None,
            other => panic!("expected a proven edge or none, got {other:?}"),
        }
    }

    #[test]
    fn rust_relative_paths_start_from_the_innermost_inline_mod() {
        with_inline_mod_context(Vec::new(), true, |ctx| {
            let at = |scope: &str, receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call(scope, receiver, callee))
            };
            // `super` in `inner` is `outer`, not the crate root the file path climbs to.
            assert_eq!(
                at("scope:t:body", "super", "f").as_deref(),
                Some("sym:outer:f")
            );
            // Two hops leave both blocks and land on the file's own module.
            assert_eq!(
                at("scope:t:body", "super::super", "f").as_deref(),
                Some("sym:worker:f")
            );
            // Three climb past the file, into its parent module: the crate root.
            assert_eq!(
                at("scope:t:body", "super::super::super", "f").as_deref(),
                Some("sym:lib:f")
            );
            assert_eq!(
                at("scope:t:body", "self", "g").as_deref(),
                Some("sym:inner:g")
            );
            // `self` in `inner` declares no `f`; the file's and `outer`'s are not in it.
            assert_eq!(at("scope:t:body", "self", "f"), None);
            // A path descends into the inline blocks it names.
            assert_eq!(
                at("scope:worker", "self::outer::inner", "g").as_deref(),
                Some("sym:inner:g")
            );
            assert_eq!(
                at("scope:t:body", "super::super::outer", "f").as_deref(),
                Some("sym:outer:f")
            );
            // The file's own `self::f` is its item, not the same-named items of its blocks.
            assert_eq!(
                at("scope:worker", "self", "f").as_deref(),
                Some("sym:worker:f")
            );
            assert_eq!(
                at("scope:worker", "super", "f").as_deref(),
                Some("sym:lib:f")
            );
        });
    }

    #[test]
    fn rust_relative_path_through_a_bodiless_mod_follows_the_module_file() {
        // `super::f` in `inner` names what `outer` declares; `outer` declares no `h`, so no item
        // of the file or the crate root may stand in for it.
        with_inline_mod_context(Vec::new(), true, |ctx| {
            let call = module_path_call("scope:t:body", "super", "h");
            assert_eq!(proven_target(ctx, &call), None);
        });
        // `mod helpers;` in `outer` is the file `src/worker/outer/helpers.rs`, and `mod child;`
        // at file level is `src/worker/child.rs`: the path continues there by name rather than
        // in the empty scope the parser gives the declaration.
        let helper = Symbol {
            id: SymbolId::new("sym:helpers:h"),
            name: "h".into(),
            qualified_name: "src::worker::outer::helpers::h".into(),
            kind: SymbolKind::Function,
            file_id: FileId::new("file:src/worker/outer/helpers.rs"),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: Some(ScopeId::new("scope:helpers:file")),
            signature: None,
            visibility: Visibility::Public,
        };
        with_inline_mod_context(vec![helper.clone()], true, |ctx| {
            let at = |scope: &str, receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call(scope, receiver, callee))
            };
            assert_eq!(
                at("scope:t:body", "super::helpers", "h").as_deref(),
                Some("sym:helpers:h")
            );
            assert_eq!(
                at("scope:worker", "self::child", "c").as_deref(),
                Some("sym:child:c")
            );
            assert_eq!(
                at("scope:t:body", "super::super::child", "c").as_deref(),
                Some("sym:child:c")
            );
        });
        // Without the declarations an empty `mod` scope may be a block or a declaration, so the
        // path proves nothing; a block that encloses scopes is still one.
        with_inline_mod_context(vec![helper], false, |ctx| {
            let at = |scope: &str, receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call(scope, receiver, callee))
            };
            assert_eq!(at("scope:t:body", "super::helpers", "h"), None);
            assert_eq!(at("scope:worker", "self::child", "c"), None);
            assert_eq!(
                at("scope:worker", "self::outer::inner", "g").as_deref(),
                Some("sym:inner:g")
            );
        });
    }

    #[test]
    fn rust_module_paths_spelled_from_file_paths_skip_misplaced_module_files() {
        // `mod child;` in `src/worker.rs`, with `src/worker/child.rs` holding `c`; the crate root
        // `src/lib.rs` declares `f`.
        let resolve = |misplaced: &[&str]| {
            with_misplaced_module_files(Vec::new(), true, misplaced, |ctx| {
                let at = |scope: &str, receiver: &str, callee: &str| {
                    proven_target(ctx, &module_path_call(scope, receiver, callee))
                };
                (
                    at("scope:worker", "self::child", "c"),
                    at("scope:worker", "crate::worker::child", "c"),
                    at("scope:worker", "super", "f"),
                    at("scope:worker", "self", "f"),
                )
            })
        };
        let placed = resolve(&[]);
        assert_eq!(placed.0.as_deref(), Some("sym:child:c"));
        assert_eq!(placed.1.as_deref(), Some("sym:child:c"));
        assert_eq!(placed.2.as_deref(), Some("sym:lib:f"));

        // `#[path = "elsewhere.rs"] mod child;` leaves `src/worker/child.rs` at the default
        // location, where no declaration places it: neither path may prove its `c`.
        let decoy = resolve(&["src/worker/child.rs"]);
        assert_eq!(decoy.0, None);
        assert_eq!(decoy.1, None);
        assert_eq!(decoy.2.as_deref(), Some("sym:lib:f"));

        // `src/worker.rs` itself mounted by `#[path]` from another module: `super` read off its
        // file path is not its parent, while an item of the file stays proven.
        let mounted = resolve(&["src/worker.rs"]);
        assert_eq!(mounted.0, None);
        assert_eq!(mounted.2, None);
        assert_eq!(mounted.3.as_deref(), Some("sym:worker:f"));
    }
}
