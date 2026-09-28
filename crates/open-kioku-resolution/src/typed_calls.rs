use crate::context::{ResolutionContext, RustRelativeModule, ScopedImport};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::{ConfiguredModule, RustModuleFiles, RustModulePlacement};
use crate::pipeline::{
    evaluate_candidates, normalize_candidates, ResolutionCandidate, ResolutionOutcome,
};
use open_kioku_core::{
    Binding, CallSite, Confidence, EvidenceSourceType, FileId, FileRange, GraphEdgeType, Language,
    LineRange, RelationshipProof, RelationshipProofKind, ScopeId, Symbol, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::{ConfiguredImportTargets, ImportBinding};
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
        let declared = if ctx.language == Language::Rust {
            rust_annotated_receiver_type(declared).unwrap_or(declared)
        } else {
            declared
        };
        return Some((declared.to_string(), true));
    }
    let inferred = binding
        .inferred_type
        .as_deref()
        .map(str::trim)
        .filter(|inferred| !inferred.is_empty())?;
    Some(inferred_receiver_type(ctx, scope_id, inferred))
}

/// The type whose members a method call on a Rust binding annotated `annotation` reaches: the
/// annotation's path without its generic arguments, seen through references. `x: Foo<u8>`,
/// `w: &Wrapper<u8>` and `m: &'a mut Foo` are a `Foo`, a `Wrapper` and a `Foo`; generic arguments
/// pick an instantiation of the type, not another type. `v: Vec<Foo>` is a `Vec`, never a `Foo`.
/// `None` for any other form, such as a tuple, slice, pointer, trait object or `impl Trait`.
fn rust_annotated_receiver_type(annotation: &str) -> Option<&str> {
    let mut rest = annotation.trim();
    while let Some(referent) = rest.strip_prefix('&') {
        rest = referent.trim_start();
        if let Some(lifetime) = rest.strip_prefix('\'') {
            let end = lifetime.find(char::is_whitespace)?;
            rest = lifetime[end..].trim_start();
        }
        if let Some(referent) = rest.strip_prefix("mut ") {
            rest = referent.trim_start();
        }
    }
    // The parser drops a parameter's leading `&`, leaving `mut Foo` for `&mut Foo`.
    if let Some(referent) = rest.strip_prefix("mut ") {
        rest = referent.trim_start();
    }
    let path = match rest.find('<') {
        Some(open) => {
            let arguments = rest[open..].trim_end();
            if !arguments.ends_with('>') || !angle_brackets_close_at_end(arguments) {
                return None;
            }
            rest[..open].trim_end()
        }
        None => rest,
    };
    let body = path.strip_prefix("::").unwrap_or(path);
    let plain = !body.is_empty()
        && body.split("::").all(|segment| {
            let segment = segment.strip_prefix("r#").unwrap_or(segment);
            !segment.is_empty() && segment.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        });
    plain.then_some(path)
}

/// Whether the `<` opening `arguments` is closed by its last character, so nothing follows the
/// generic arguments: `<u8>` but not `<u8> + Send` or `<A>::B<C>`.
fn angle_brackets_close_at_end(arguments: &str) -> bool {
    let mut depth = 0usize;
    for (index, ch) in arguments.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
                if depth == 0 {
                    return index + ch.len_utf8() == arguments.len();
                }
            }
            _ => {}
        }
    }
    false
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

/// A Rust call through a `crate::`, `self::` or `super::` path, or one through a crate name the
/// caller's package declares (see [`rust_crate_name_member_names`]). `self` and `super` start
/// from the innermost module around the call, an inline `mod` block included: `super::f()` in
/// `mod tests` of `src/worker.rs` names the `f` that file declares, not one in the crate root. A
/// path ending in a module of this file names an item that module declares or brings in from
/// another module of the file. One ending outside the file's scopes names items by the qualified
/// names its module path spells in the caller's own crate, read from the declared module tree: it
/// starts only in a file the tree records, `crate::` from that crate's root and `self`/`super`
/// only from a file the tree places at its path, and ends only in a file placed in that crate.
///
/// When the whole path names no item, a path whose last segment names a type of the module the
/// rest reaches is a call to an associated function of that type: `crate::a::Engine::new()` and
/// `engine::Engine::new()` reach `new` declared in an `impl` of that `Engine`.
fn resolve_rust_qualified_module_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    receiver: &str,
) -> Option<ResolutionOutcome> {
    let receiver = receiver.trim();
    if let Some((targets, strategy)) =
        rust_module_path_items(call, ctx, receiver, &call.callee_name, |symbol| {
            !matches!(symbol.kind, SymbolKind::Module | SymbolKind::Package)
        })
    {
        return Some(rust_module_path_outcome(call, ctx, targets, strategy));
    }
    let (module_path, type_name) = receiver.rsplit_once("::")?;
    let type_name = type_name.trim();
    if matches!(type_name, "" | "self" | "super" | "crate") {
        return None;
    }
    let (types, strategy) =
        rust_module_path_items(call, ctx, module_path.trim(), type_name, |symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
            )
        })?;
    let mut members = types
        .iter()
        .flat_map(|type_id| find_members_by_name(ctx, type_id, &call.callee_name))
        .filter(|member| {
            ctx.symbols.get(member).is_some_and(|symbol| {
                matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
            })
        })
        .collect::<Vec<_>>();
    normalize_symbol_ids(&mut members);
    if members.is_empty() {
        return None;
    }
    Some(rust_type_path_outcome(call, ctx, members, strategy))
}

/// The items named `name` that `accept` admits in the module a Rust path names, and how the path
/// reached them; `None` when the path is not one the resolver reads or reaches no such item.
fn rust_module_path_items(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    path: &str,
    name: &str,
    accept: impl Fn(&Symbol) -> bool,
) -> Option<(Vec<SymbolId>, RustModulePathStrategy)> {
    if let Some(found) = rust_crate_name_items(call, ctx, path, name, &accept) {
        return Some(found);
    }
    let own = ctx.scopes.rust_module_placement(ctx.file_id);
    // A file another crate may compile too is read against no one crate.
    let mut placement = own.filter(|placement| !placement.in_other_crates);
    let module = if path == "crate" || path.starts_with("crate::") {
        rust_crate_path_module(path)?
    } else {
        let (depth, segments) = rust_relative_path(path)?;
        match crate::context::rust_relative_module(ctx, &call.scope_id, depth, &segments)? {
            RustRelativeModule::InFile(module) => {
                let mut targets = crate::context::rust_module_items(ctx, module, name, &accept);
                normalize_symbol_ids(&mut targets);
                return (!targets.is_empty())
                    .then_some((targets, RustModulePathStrategy::ModuleScope));
            }
            RustRelativeModule::Outside { climbs, path } => {
                // A path that climbs no higher than this file's own module ends below it, in the
                // same file in every crate that compiles this one at the same place.
                if climbs == 0 {
                    placement = own.filter(|placement| {
                        !placement.in_other_crates || placement.own_subtree_in_every_crate
                    });
                }
                // The path is read off this file's module, which only a placed file has.
                rust_outside_module(placement?, climbs, &path)?
            }
        }
    };
    let placement = placement?;
    let names = rust_module_member_names(placement, &module, name);
    let mut targets = rust_qualified_targets(ctx, &names, &accept);
    // A file the module tree does not place where its path says, such as the default location
    // of a `#[path]` module, or one of another crate, is not the module the path spells.
    targets.retain(|target| {
        ctx.symbols
            .get(target)
            .is_some_and(|symbol| ctx.scopes.is_placed_in_crate_of(&symbol.file_id, placement))
    });
    if let Some(configured) =
        ctx.scopes
            .rust_configured_module(placement, placement.module.as_deref(), &module)
    {
        return rust_configured_items(ctx, configured, targets, name, &accept);
    }
    normalize_symbol_ids(&mut targets);
    (!targets.is_empty()).then_some((targets, RustModulePathStrategy::CrateQualified))
}

/// The items named `name` that `accept` admits in a module whose file configuration selects:
/// those of each file that may hold it, or, for a path below such a module that no file module
/// holds, `placed`, the items the placed files hold. None of them is proven.
fn rust_configured_items(
    ctx: &ResolutionContext<'_>,
    configured: ConfiguredModule<'_>,
    placed: Vec<SymbolId>,
    name: &str,
    accept: impl Fn(&Symbol) -> bool,
) -> Option<(Vec<SymbolId>, RustModulePathStrategy)> {
    let (mut targets, files) = match configured {
        ConfiguredModule::Files(files) => {
            // Tree-sitter spells an item's qualified name from its file's path, wherever the
            // module tree places the file.
            let names = files
                .files
                .iter()
                .map(|file| format!("{}::{name}", file.replace('/', "::")))
                .collect::<Vec<_>>();
            (rust_qualified_targets(ctx, &names, accept), files)
        }
        ConfiguredModule::Below(files) => (placed, files),
    };
    normalize_symbol_ids(&mut targets);
    (!targets.is_empty()).then(|| (targets, RustModulePathStrategy::Configured(files.clone())))
}

/// The associated functions a Rust path through a type reached, each proven by the call site,
/// the module path, the qualified name of the type it spells, and membership in that type.
fn rust_type_path_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
    strategy: RustModulePathStrategy,
) -> ResolutionOutcome {
    let (module_strategy, type_strategy) = match &strategy {
        RustModulePathStrategy::CrateQualified => {
            ("rust_crate_qualified_module", "rust_crate_qualified_type")
        }
        RustModulePathStrategy::ModuleScope => ("rust_module_scope_path", "rust_module_scope_type"),
        RustModulePathStrategy::CrateName => ("rust_crate_name_module", "rust_crate_name_type"),
        RustModulePathStrategy::Configured(_) => ("rust_configured_module", "rust_configured_type"),
    };
    let candidate_count = targets.len();
    let (confidence, ambiguity, message) = match &strategy {
        RustModulePathStrategy::Configured(files) => (
            Confidence::High,
            configured_file_names(files),
            configured_message("associated function of the type a Rust path names", files),
        ),
        _ => (
            Confidence::Exact,
            ambiguity_strings(&targets),
            "associated function of the type an exact Rust path names".to_string(),
        ),
    };
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), confidence);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::LexicalScope,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: message.clone(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            for (kind, strategy) in [
                (
                    RelationshipProofKind::ModuleOrPackageBinding,
                    module_strategy,
                ),
                (RelationshipProofKind::QualifiedName, type_strategy),
                (
                    RelationshipProofKind::ContainingType,
                    "direct_member_of_path_type",
                ),
            ] {
                candidate.proofs.push(proof(
                    kind,
                    strategy,
                    call,
                    ctx,
                    &target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();
    rust_path_outcome(&strategy, candidates)
}

/// The outcome of the candidates a Rust path reached: proven when exactly one is, or, through a
/// module whose file configuration selects, every one kept as an alternative.
fn rust_path_outcome(
    strategy: &RustModulePathStrategy,
    candidates: Vec<ResolutionCandidate>,
) -> ResolutionOutcome {
    match strategy {
        RustModulePathStrategy::Configured(files) => ResolutionOutcome::Alternatives {
            candidates: normalize_candidates(candidates),
            reason: configured_message("the Rust path", files),
        },
        _ => evaluate_candidates(&GraphEdgeType::Calls, candidates),
    }
}

/// The files a module configuration selects may be compiled from, as a proof's ambiguity names
/// them: each such proof is short of unique, so the relationship is not authoritative.
pub(crate) fn configured_file_names(files: &RustModuleFiles) -> Vec<String> {
    let mut names = files
        .files
        .iter()
        .map(|file| format!("{file}.rs"))
        .collect::<Vec<_>>();
    if files.unread {
        names.push("a `path` attribute the index cannot read".into());
    }
    names
}

/// The caveat of a candidate reached through a module whose file configuration selects.
pub(crate) fn configured_message(what: &str, files: &RustModuleFiles) -> String {
    format!(
        "{what} reaches a module whose file configuration selects, one of {}; the call reaches this candidate only on builds that compile its file",
        configured_file_names(files).join(", ")
    )
}

/// The files a Rust import names through a module whose file configuration selects (#615).
pub(crate) fn configured_import_files(targets: &ConfiguredImportTargets) -> RustModuleFiles {
    RustModuleFiles {
        files: targets.files.clone(),
        unread: targets.unread,
    }
}

/// The candidates a call through a Rust import reached, when the import names a module whose
/// file configuration selects or an item of one (#615): as for a path spelling that module, each
/// file's candidate is kept, at `High` confidence, with import and qualified-name proofs that
/// list `files` as ambiguity, so none is proven.
pub(crate) fn rust_configured_import_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    mut targets: Vec<SymbolId>,
    files: &RustModuleFiles,
    evidence_kind: ResolutionEvidenceKind,
    import_strategy: &str,
    member_strategy: &str,
) -> ResolutionOutcome {
    normalize_symbol_ids(&mut targets);
    let candidate_count = targets.len();
    let ambiguity = configured_file_names(files);
    let message = configured_message("a Rust import", files);
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::High);
            candidate.evidence.push(ResolutionEvidence {
                kind: evidence_kind.clone(),
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: message.clone(),
            });
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            for (kind, strategy) in [
                (RelationshipProofKind::ImportBinding, import_strategy),
                (RelationshipProofKind::QualifiedName, member_strategy),
            ] {
                candidate.proofs.push(proof(
                    kind,
                    strategy,
                    call,
                    ctx,
                    &target,
                    candidate_count,
                    &ambiguity,
                ));
            }
            candidate
        })
        .collect();
    ResolutionOutcome::Alternatives {
        candidates: normalize_candidates(candidates),
        reason: configured_message("the Rust import", files),
    }
}

/// The candidates a Rust module path reached, each proven by the call site, the module path and
/// the qualified name it spells.
fn rust_module_path_outcome(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
    strategy: RustModulePathStrategy,
) -> ResolutionOutcome {
    let (message, module_strategy, member_strategy) = match &strategy {
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
        RustModulePathStrategy::CrateName => (
            "candidate from exact Rust path through a crate name the caller's package declares",
            "rust_crate_name_module",
            "rust_crate_name_member",
        ),
        RustModulePathStrategy::Configured(_) => (
            "candidate from a Rust path",
            "rust_configured_module",
            "rust_configured_member",
        ),
    };
    let candidate_count = targets.len();
    let (confidence, ambiguity, message) = match &strategy {
        RustModulePathStrategy::Configured(files) => (
            Confidence::High,
            configured_file_names(files),
            configured_message(message, files),
        ),
        _ => (
            Confidence::Exact,
            ambiguity_strings(&targets),
            message.to_string(),
        ),
    };
    let candidates = targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), confidence);
            candidate.evidence.push(ResolutionEvidence {
                kind: ResolutionEvidenceKind::LexicalScope,
                source_type: EvidenceSourceType::TreeSitter,
                file_range: call_file_range(call, ctx),
                symbol_id: Some(target.clone()),
                message: message.clone(),
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
    rust_path_outcome(&strategy, candidates)
}

/// The items named `name` that `accept` admits in the module a Rust path through a crate name the
/// caller's package declares reaches, read from that library crate's root and ending only in a
/// file placed in that crate. Only a module receiver reaches here: `engine.run()` shares the
/// receiver text of `engine::run()`, and a closure, pattern or loop binding named `engine` is not
/// recorded as a binding.
fn rust_crate_name_items(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    path: &str,
    name: &str,
    accept: impl Fn(&Symbol) -> bool,
) -> Option<(Vec<SymbolId>, RustModulePathStrategy)> {
    let (module, crate_placement) = rust_crate_name_module(call, ctx, path.trim())?;
    let names = rust_module_member_names(crate_placement, &module, name);
    let mut targets = rust_qualified_targets(ctx, &names, &accept);
    targets.retain(|target| {
        ctx.symbols.get(target).is_some_and(|symbol| {
            ctx.scopes
                .is_placed_in_crate_of(&symbol.file_id, crate_placement)
        })
    });
    // The caller is in another crate, so no choice of the library's is made by compiling it.
    if let Some(configured) = ctx
        .scopes
        .rust_configured_module(crate_placement, None, &module)
    {
        return rust_configured_items(ctx, configured, targets, name, &accept);
    }
    normalize_symbol_ids(&mut targets);
    (!targets.is_empty()).then_some((targets, RustModulePathStrategy::CrateName))
}

/// How a Rust module path reached its candidates.
enum RustModulePathStrategy {
    /// Items of each file that may hold a module whose file configuration selects, or below one
    /// (#613): the path proves none of them.
    Configured(RustModuleFiles),
    /// Qualified names the path spells from the file path.
    CrateQualified,
    /// Items a module scope of this file declares.
    ModuleScope,
    /// Qualified names the path spells in the library crate its first segment names: a
    /// dependency the caller's package declares, or that package's own library.
    CrateName,
}

/// The module a path through a crate name reaches, below the root of the library crate it starts
/// in, and that crate: `engine::plan::f()` where the caller's package declares `engine`.
/// The first segment names that crate only when nothing of this file does: an explicit import of
/// the name, or an item of it in lexical scope, such as a module, shadows the crate. A glob
/// importing a module of that name from another file is not seen.
fn rust_crate_name_module<'c>(
    call: &CallSite,
    ctx: &ResolutionContext<'c>,
    receiver: &str,
) -> Option<(Vec<String>, &'c RustModulePlacement)> {
    let mut segments = receiver.split("::").map(str::trim);
    let first = segments.next()?;
    let placement = ctx.scopes.rust_named_crate(ctx.file_id, first)?;
    let explicitly_imported = ctx
        .repository
        .imports
        .by_file_local_name
        .get(&(ctx.file_id.clone(), first.to_string()))
        .is_some_and(|bindings| bindings.iter().any(|binding| !binding.is_glob));
    if explicitly_imported
        || crate::context::nearest_lexical_items(ctx, &call.scope_id, first, |_| true).is_some()
    {
        return None;
    }
    let mut module = Vec::new();
    for segment in segments {
        if matches!(segment, "" | "self" | "super" | "crate") {
            return None;
        }
        module.push(rust_module_name(segment).to_string());
    }
    Some((module, placement))
}

/// The symbols of the qualified names a Rust path spells that `accept` admits. A call path never
/// ends in a module: `task::yield_now()` beside `mod yield_now;` names a function, and the
/// module's own symbol carries the same qualified name as an item of its parent would.
fn rust_qualified_targets(
    ctx: &ResolutionContext<'_>,
    names: &[String],
    accept: impl Fn(&Symbol) -> bool,
) -> Vec<SymbolId> {
    names
        .iter()
        .filter_map(|name| ctx.symbols.by_qualified.get(name))
        .flat_map(|ids| ids.iter().cloned())
        .filter(|id| ctx.symbols.get(id).is_some_and(&accept))
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

/// The module a `crate::` path names, below the caller's crate root.
fn rust_crate_path_module(receiver: &str) -> Option<Vec<String>> {
    let module = match receiver.strip_prefix("crate::") {
        None if receiver == "crate" => Vec::new(),
        None => return None,
        Some(path) => {
            let mut module = Vec::new();
            for segment in path.split("::").map(str::trim) {
                if matches!(segment, "" | "self" | "super" | "crate") {
                    return None;
                }
                module.push(rust_module_name(segment).to_string());
            }
            module
        }
    };
    Some(module)
}

/// The module `climbs` modules above the caller's own module and then down `path`, below the
/// caller's crate root. `None` when the caller is not placed at its path or the climb leaves the
/// crate.
fn rust_outside_module(
    placement: &RustModulePlacement,
    climbs: usize,
    path: &[String],
) -> Option<Vec<String>> {
    let own = placement.module.as_ref()?;
    let kept = own.len().checked_sub(climbs)?;
    let module = own[..kept]
        .iter()
        .map(String::as_str)
        .chain(path.iter().map(|segment| rust_module_name(segment)))
        .map(str::to_string)
        .collect::<Vec<_>>();
    Some(module)
}

/// Qualified names of `callee` in `module` of the crate, as tree-sitter spells them from module
/// file paths: `<dir>/<module>.rs` or `<dir>/<module>/mod.rs`, or the crate root files.
fn rust_module_member_names(
    placement: &RustModulePlacement,
    module: &[String],
    callee: &str,
) -> Vec<String> {
    let mut names = if module.is_empty() {
        placement
            .crate_roots
            .iter()
            .map(|root| format!("{root}::{callee}"))
            .collect()
    } else {
        // `crate_dir` is empty for a module tree at the repository root.
        let module = [placement.crate_dir.as_str()]
            .into_iter()
            .filter(|dir| !dir.is_empty())
            .chain(module.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("::");
        vec![
            format!("{module}::{callee}"),
            format!("{module}::mod::{callee}"),
        ]
    };
    names.sort();
    names.dedup();
    names
}

/// The module name a path segment spells: `r#type` is the module `type`, in `type.rs`.
fn rust_module_name(segment: &str) -> &str {
    segment.strip_prefix("r#").unwrap_or(segment)
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
        binding.target_file.is_some()
            || binding.resolved_module.is_some()
            || configured_module_files(binding).is_some()
    }) {
        ScopedImport::Resolved(bindings) => bindings,
        ScopedImport::NotImported | ScopedImport::Unresolved => Vec::new(),
    };
    let members_in_file = |file: &FileId, targets: &mut Vec<SymbolId>| {
        for id in ctx.symbols.by_file.get(file).into_iter().flatten() {
            if ctx
                .symbols
                .get(id)
                .map(|symbol| {
                    // `mod yield_now;` beside `pub use yield_now::yield_now;` is a module of the
                    // same name, never what a call reaches.
                    symbol.name == call.callee_name
                        && symbol.parent_symbol_id.is_none()
                        && !matches!(symbol.kind, SymbolKind::Module | SymbolKind::Package)
                })
                .unwrap_or(false)
            {
                targets.push(id.clone());
            }
        }
    };

    // A Rust import of a module whose file configuration selects names each of its files, and
    // a call through it proves none of their items (#615).
    if import_bindings
        .iter()
        .any(|binding| configured_module_files(binding).is_some())
    {
        let mut files = RustModuleFiles::default();
        for binding in &import_bindings {
            match configured_module_files(binding) {
                Some(configured) => {
                    for file in &configured.module_files {
                        members_in_file(file, &mut targets);
                    }
                    let found = configured_import_files(configured);
                    files.files.extend(found.files);
                    files.unread |= found.unread;
                }
                None => {
                    if let Some(file) = &binding.target_file {
                        members_in_file(file, &mut targets);
                    }
                }
            }
        }
        files.files.sort();
        files.files.dedup();
        if targets.is_empty() {
            return evaluate_candidates(&GraphEdgeType::Calls, Vec::new());
        }
        return rust_configured_import_outcome(
            call,
            ctx,
            targets,
            &files,
            ResolutionEvidenceKind::ExplicitImport,
            "rust_configured_receiver_import",
            "rust_configured_receiver_member",
        );
    }

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
            members_in_file(target_file, &mut targets);
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

/// The files a Rust module import names when configuration selects the module's file.
fn configured_module_files(binding: &ImportBinding) -> Option<&ConfiguredImportTargets> {
    binding
        .configured_targets
        .as_ref()
        .filter(|configured| !configured.module_files.is_empty())
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
    use std::collections::HashMap;

    fn placement(crate_dir: &str, roots: &[&str], module: Option<&[&str]>) -> RustModulePlacement {
        RustModulePlacement {
            crate_dir: crate_dir.into(),
            crate_roots: roots.iter().map(|root| root.to_string()).collect(),
            module: module.map(|module| module.iter().map(|name| name.to_string()).collect()),
            in_other_crates: false,
            own_subtree_in_every_crate: true,
        }
    }

    /// Qualified names of `callee` in the module a `crate::` path names in the caller's crate.
    fn rust_crate_path_member_names(
        placement: &RustModulePlacement,
        receiver: &str,
        callee: &str,
    ) -> Option<Vec<String>> {
        let module = rust_crate_path_module(receiver)?;
        Some(rust_module_member_names(placement, &module, callee))
    }

    /// Qualified names of `callee` in the module `climbs` modules above the caller's own module
    /// and then down `path`.
    fn rust_outside_member_names(
        placement: &RustModulePlacement,
        climbs: usize,
        path: &[String],
        callee: &str,
    ) -> Option<Vec<String>> {
        let module = rust_outside_module(placement, climbs, path)?;
        Some(rust_module_member_names(placement, &module, callee))
    }

    #[test]
    fn rust_module_symbol_names_of_a_tree_at_the_repository_root_have_no_leading_separator() {
        // A package at the repository root with `[lib] path = "lib.rs"` beside
        // `[[bin]] path = "main.rs"` keeps its modules in `""`: `tools/mod.rs` is `tools::mod`.
        let root = placement("", &["lib"], Some(&[]));
        assert_eq!(
            rust_crate_path_member_names(&root, "crate::tools", "t").unwrap(),
            vec!["tools::mod::t".to_string(), "tools::t".to_string()]
        );
        let inner = placement("", &["lib"], Some(&["tools", "inner"]));
        assert_eq!(
            rust_outside_member_names(&inner, 1, &[], "t").unwrap(),
            vec!["tools::mod::t".to_string(), "tools::t".to_string()]
        );
    }

    #[test]
    fn rust_module_symbol_names_match_tree_sitter_qualified_names() {
        let root = placement("src", &["src::lib"], Some(&[]));
        assert_eq!(
            rust_crate_path_member_names(&root, "crate::storage", "persist").unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_crate_path_member_names(&root, "crate", "persist").unwrap(),
            vec!["src::lib::persist".to_string()]
        );
        assert_eq!(
            rust_crate_path_member_names(&root, "crate::super", "p"),
            None
        );
        let service = placement("src", &["src::lib"], Some(&["storage", "service"]));
        assert_eq!(
            rust_outside_member_names(&service, 1, &[], "persist").unwrap(),
            vec![
                "src::storage::mod::persist".to_string(),
                "src::storage::persist".to_string(),
            ]
        );
        let worker = placement("src", &["src::lib", "src::main"], Some(&["worker"]));
        assert_eq!(
            rust_outside_member_names(&worker, 1, &[], "persist").unwrap(),
            vec![
                "src::lib::persist".to_string(),
                "src::main::persist".to_string(),
            ]
        );
        assert_eq!(
            rust_outside_member_names(
                &worker,
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
        // A climb past the crate root leaves the crate.
        assert_eq!(rust_outside_member_names(&root, 1, &[], "persist"), None);
        // A file the tree does not place has no module to climb from.
        let unplaced = placement("src", &["src::lib"], None);
        assert_eq!(
            rust_outside_member_names(&unplaced, 1, &[], "persist"),
            None
        );
    }

    #[test]
    fn rust_crate_paths_are_spelled_from_the_callers_own_crate() {
        // A workspace member's `crate::util` is its own `crates/a/src/util.rs`, never the root
        // package's `src/util.rs`.
        let member = placement("crates::a::src", &["crates::a::src::lib"], Some(&[]));
        assert_eq!(
            rust_crate_path_member_names(&member, "crate::util", "f").unwrap(),
            vec![
                "crates::a::src::util::f".to_string(),
                "crates::a::src::util::mod::f".to_string(),
            ]
        );
        assert_eq!(
            rust_crate_path_member_names(&member, "crate", "f").unwrap(),
            vec!["crates::a::src::lib::f".to_string()]
        );
        // `pub mod r#type;` is the file `type.rs`.
        assert_eq!(
            rust_crate_path_member_names(&member, "crate::r#type", "ty").unwrap(),
            vec![
                "crates::a::src::type::mod::ty".to_string(),
                "crates::a::src::type::ty".to_string(),
            ]
        );
        let nested = placement("crates::a::src", &["crates::a::src::lib"], Some(&["type"]));
        assert_eq!(
            rust_outside_member_names(&nested, 1, &["r#match".to_string()], "m").unwrap(),
            vec![
                "crates::a::src::match::m".to_string(),
                "crates::a::src::match::mod::m".to_string(),
            ]
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
        with_annotated_context("Service", symbols, test)
    }

    /// As [`with_context`], with the binding `svc` annotated `declared_type`.
    fn with_annotated_context<T>(
        declared_type: &str,
        symbols: Vec<Symbol>,
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
            declared_type: Some(declared_type.into()),
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
    fn rust_annotated_receiver_types_drop_generic_arguments_and_references() {
        for (annotation, expected) in [
            ("Service", Some("Service")),
            ("Service<u8>", Some("Service")),
            ("&Service<u8>", Some("Service")),
            ("&'a mut Service<'a, T>", Some("Service")),
            ("mut Service", Some("Service")),
            ("&&Service", Some("Service")),
            ("net::Service<Vec<u8>>", Some("net::Service")),
            ("Vec<Service>", Some("Vec")),
            ("&dyn Service", None),
            ("impl Service", None),
            ("[Service; 2]", None),
            ("(Service, u8)", None),
            ("*const Service", None),
            ("Service<u8> + Send", None),
            ("<Service as Run>::Output", None),
            ("Service<fn() -> u8>", None),
        ] {
            assert_eq!(
                rust_annotated_receiver_type(annotation),
                expected,
                "{annotation}"
            );
        }
    }

    #[test]
    fn rust_generic_and_reference_annotations_type_their_receiver() {
        for annotation in [
            "Service<u8>",
            "&Service<u8>",
            "&'a mut Service",
            "mut Service",
        ] {
            with_annotated_context(
                annotation,
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
                            .any(|proof| proof.kind == RelationshipProofKind::ReceiverType));
                    }
                    other => panic!("`{annotation}` should prove the call, got {other:?}"),
                },
            );
        }
    }

    #[test]
    fn rust_annotation_of_a_container_does_not_type_the_receiver_as_its_element() {
        with_annotated_context(
            "Vec<Service>",
            vec![
                type_symbol("symbol:type:Service", "Service"),
                method_symbol("symbol:method:Service.run", "symbol:type:Service"),
            ],
            |ctx| match resolve_typed_receiver_outcome(&call(), ctx) {
                ResolutionOutcome::Unresolved { candidates, .. } => assert!(candidates.is_empty()),
                other => panic!("`Vec<Service>` is no `Service`, got {other:?}"),
            },
        );
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

    /// [`with_inline_mod_context`], with the crate root `src/lib.rs`, `src/worker.rs`,
    /// `src/worker/child.rs` and `src/worker/outer/helpers.rs` recorded as placed at their paths
    /// in the library crate, except `misplaced`, which the declared module tree does not place.
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
                path_attributes: Vec::new(),
                path_is_conditional: false,
                range: range(line),
            };
            scopes.record_module_declarations(&[
                declaration("scope:worker", "child", false, 2),
                declaration("scope:worker", "outer", true, 3),
                declaration("scope:outer", "helpers", false, 4),
                declaration("scope:outer", "inner", true, 5),
            ]);
        }
        scopes.record_rust_module_placements(
            [
                ("src/lib.rs", &[][..]),
                ("src/worker.rs", &["worker"][..]),
                ("src/worker/child.rs", &["worker", "child"][..]),
                (
                    "src/worker/outer/helpers.rs",
                    &["worker", "outer", "helpers"][..],
                ),
            ]
            .into_iter()
            .map(|(path, module)| {
                let module = (!misplaced.contains(&path)).then_some(module);
                (
                    FileId::new(format!("file:{path}")),
                    placement("src", &["src::lib"], module),
                )
            })
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

    /// A context calling from the crate root file `caller`, with one function per
    /// `(qualified name, file)` in `items` and `placements` recorded. Each symbol's id is its
    /// qualified name.
    fn with_rust_files<T>(
        caller: &str,
        items: &[(&str, &str)],
        placements: Vec<(&str, RustModulePlacement)>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        with_rust_crates(caller, items, placements, Vec::new(), test)
    }

    /// [`with_rust_files`] where the caller names the library crates `crates` by crate name.
    fn with_rust_crates<T>(
        caller: &str,
        items: &[(&str, &str)],
        placements: Vec<(&str, RustModulePlacement)>,
        crates: Vec<(&str, RustModulePlacement)>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        with_rust_modules(caller, items, placements, crates, HashMap::new(), test)
    }

    /// [`with_rust_crates`] with the modules configuration selects a file for, by crate root.
    fn with_rust_modules<T>(
        caller: &str,
        items: &[(&str, &str)],
        placements: Vec<(&str, RustModulePlacement)>,
        crates: Vec<(&str, RustModulePlacement)>,
        configured: HashMap<String, crate::index::RustConfiguredModules>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        let caller_id = FileId::new(format!("file:{caller}"));
        let mut scopes = ScopeIndex::build(vec![Scope {
            id: ScopeId::new("scope:worker"),
            file_id: caller_id.clone(),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::File,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 40,
                end_column: 1,
            },
        }]);
        scopes.record_rust_module_placements(
            placements
                .into_iter()
                .map(|(path, placement)| (FileId::new(format!("file:{path}")), placement))
                .collect(),
        );
        scopes.record_rust_configured_modules(configured);
        scopes.record_rust_crate_names(
            [(
                caller_id.clone(),
                std::sync::Arc::new(
                    crates
                        .into_iter()
                        .map(|(name, placement)| (name.to_string(), placement))
                        .collect(),
                ),
            )]
            .into(),
        );
        // An item written `path (mod)` is a module symbol of that qualified name, `path (type)`
        // a struct, and `path @Type` a method of the struct whose id is `Type`, with the qualified
        // name tree-sitter gives an `impl` member: the file's path and the member's name.
        let symbols = items
            .iter()
            .map(|(item, file)| {
                let (qualified, kind, parent) = if let Some(module) = item.strip_suffix(" (mod)") {
                    (module, SymbolKind::Module, None)
                } else if let Some(ty) = item.strip_suffix(" (type)") {
                    (ty, SymbolKind::Class, None)
                } else if let Some((member, owner)) = item.split_once(" @") {
                    (member, SymbolKind::Method, Some(owner))
                } else {
                    (*item, SymbolKind::Function, None)
                };
                let name = qualified.rsplit("::").next().unwrap_or_default();
                Symbol {
                    id: SymbolId::new(match (&kind, parent) {
                        (SymbolKind::Module, _) => format!("{qualified}#mod"),
                        (_, Some(owner)) => format!("{owner}.{name}"),
                        _ => qualified.to_string(),
                    }),
                    name: name.into(),
                    qualified_name: qualified.into(),
                    kind,
                    file_id: FileId::new(format!("file:{file}")),
                    range: None,
                    language: Language::Rust,
                    confidence: Confidence::Exact,
                    provenance: EvidenceSourceType::TreeSitter,
                    module_id: None,
                    parent_symbol_id: parent.map(SymbolId::new),
                    scope_id: None,
                    signature: None,
                    visibility: Visibility::Public,
                }
            })
            .collect();
        let symbol_index = SymbolIndex::build(symbols);
        let bindings = BindingIndex::build(Vec::new());
        let inheritance = InheritanceIndex::build(Vec::new());
        let repository = open_kioku_semantic_model::SemanticRepository::new();
        let semantics = open_kioku_languages::semantics_for(&Language::Rust).unwrap();
        let context = ResolutionContext::new(
            &caller_id,
            std::path::Path::new(caller),
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
    fn a_rust_path_into_a_module_whose_file_configuration_selects_keeps_every_file_unproven() {
        // `src/sys/mod.rs` declares `#[cfg_attr(windows, path = "win.rs")] mod imp;`: `imp` is
        // `sys/imp.rs`, placed at its path, or `sys/win.rs`, which is not. Each declares `inner`.
        let at = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let items = [
            ("src::sys::imp::f", "src/sys/imp.rs"),
            ("src::sys::win::f", "src/sys/win.rs"),
            ("src::sys::imp::inner::g", "src/sys/imp/inner.rs"),
            ("src::sys::inner::g", "src/sys/inner.rs"),
            ("src::sys::imp::Engine (type)", "src/sys/imp.rs"),
            (
                "src::sys::imp::new @src::sys::imp::Engine",
                "src/sys/imp.rs",
            ),
            ("src::sys::f", "src/sys/mod.rs"),
        ];
        let placements = vec![
            ("src/lib.rs", at(&[])),
            ("src/sys/mod.rs", at(&["sys"])),
            ("src/sys/imp.rs", at(&["sys", "imp"])),
            ("src/sys/imp/inner.rs", at(&["sys", "imp", "inner"])),
            ("src/sys/win.rs", placement("src", &["src::lib"], None)),
            ("src/sys/inner.rs", placement("src", &["src::lib"], None)),
        ];
        let module = |path: &str| path.split("::").map(str::to_string).collect::<Vec<_>>();
        let files = |files: &[&str]| crate::index::RustModuleFiles {
            files: files.iter().map(|file| file.to_string()).collect(),
            unread: false,
        };
        let configured = HashMap::from([(
            "src::lib".to_string(),
            crate::index::RustConfiguredModules {
                choices: [module("sys::imp")].into(),
                files: [
                    (module("sys::imp"), files(&["src/sys/imp", "src/sys/win"])),
                    (
                        module("sys::imp::inner"),
                        files(&["src/sys/imp/inner", "src/sys/inner"]),
                    ),
                ]
                .into(),
            },
        )]);
        let alternatives = |outcome: ResolutionOutcome| match outcome {
            ResolutionOutcome::Alternatives { candidates, .. } => candidates
                .into_iter()
                .map(|candidate| {
                    assert_eq!(candidate.confidence, Confidence::High);
                    assert_ne!(
                        candidate.authority(&GraphEdgeType::Calls),
                        open_kioku_core::RelationshipAuthority::Authoritative
                    );
                    assert!(candidate.proofs.iter().any(|proof| proof.ambiguity
                        == vec!["src/sys/imp.rs".to_string(), "src/sys/win.rs".to_string()]
                        || proof.ambiguity
                            == vec![
                                "src/sys/imp/inner.rs".to_string(),
                                "src/sys/inner.rs".to_string()
                            ]));
                    candidate.target_symbol_id.0
                })
                .collect::<Vec<_>>(),
            other => panic!("expected alternatives, got {other:?}"),
        };
        with_rust_modules(
            "src/lib.rs",
            &items,
            placements.clone(),
            Vec::new(),
            configured.clone(),
            |ctx| {
                let outcome = |receiver: &str, callee: &str| {
                    resolve_module_member_outcome(
                        &module_path_call("scope:worker", receiver, callee),
                        ctx,
                    )
                };
                // Into the module, and below it, each file's item is kept, and none proven.
                assert_eq!(
                    alternatives(outcome("crate::sys::imp", "f")),
                    vec!["src::sys::imp::f", "src::sys::win::f"]
                );
                assert_eq!(
                    alternatives(outcome("crate::sys::imp::inner", "g")),
                    vec!["src::sys::imp::inner::g", "src::sys::inner::g"]
                );
                // A path through a type below the choice is not proven either.
                assert_eq!(
                    alternatives(outcome("crate::sys::imp::Engine", "new")),
                    vec!["src::sys::imp::Engine.new"]
                );
                // Beside the choice nothing changes.
                assert_eq!(
                    proven_target(ctx, &module_path_call("scope:worker", "crate::sys", "f"))
                        .as_deref(),
                    Some("src::sys::f")
                );
            },
        );
        // `sys/imp.rs` is compiled only when `imp` is that file, so `self::inner` there is
        // `sys/imp/inner.rs` alone.
        with_rust_modules(
            "src/sys/imp.rs",
            &items,
            placements.clone(),
            Vec::new(),
            configured.clone(),
            |ctx| {
                assert_eq!(
                    proven_target(ctx, &module_path_call("scope:worker", "self::inner", "g"))
                        .as_deref(),
                    Some("src::sys::imp::inner::g")
                );
            },
        );
        // Without the choice recorded, the placed file alone is the proven target.
        with_rust_files("src/lib.rs", &items, placements, |ctx| {
            assert_eq!(
                proven_target(
                    ctx,
                    &module_path_call("scope:worker", "crate::sys::imp", "f")
                )
                .as_deref(),
                Some("src::sys::imp::f")
            );
        });
    }

    #[test]
    fn rust_crate_paths_in_a_workspace_member_resolve_within_that_member() {
        // A root package `src/` beside member `crates/a`, both declaring `util`; only the root
        // package declares `only_root`.
        let root_pkg = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let member =
            |module: &[&str]| placement("crates::a::src", &["crates::a::src::lib"], Some(module));
        let items = [
            ("src::util::f", "src/util.rs"),
            ("src::only_root::z", "src/only_root.rs"),
            ("crates::a::src::util::f", "crates/a/src/util.rs"),
        ];
        let placements = vec![
            ("src/lib.rs", root_pkg(&[])),
            ("src/util.rs", root_pkg(&["util"])),
            ("src/only_root.rs", root_pkg(&["only_root"])),
            ("crates/a/src/lib.rs", member(&[])),
            ("crates/a/src/util.rs", member(&["util"])),
        ];
        with_rust_files("crates/a/src/lib.rs", &items, placements.clone(), |ctx| {
            let at = |receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
            };
            assert_eq!(
                at("crate::util", "f").as_deref(),
                Some("crates::a::src::util::f")
            );
            assert_eq!(at("crate::only_root", "z"), None);
        });
        // A file outside every recorded module tree, such as an integration test, has no crate
        // the path could be read against.
        with_rust_files("crates/a/tests/it.rs", &items, placements, |ctx| {
            let call = module_path_call("scope:worker", "crate::util", "f");
            assert_eq!(proven_target(ctx, &call), None);
        });
    }

    #[test]
    fn rust_paths_through_a_declared_crate_name_resolve_in_that_crates_library() {
        // `crates/app` declares `engine`; a second workspace has an `engine` of its own, and
        // `engine`'s `plan` module is declared by its library, `stray` by nothing.
        let engine = |module: &[&str]| {
            placement(
                "crates::engine::src",
                &["crates::engine::src::lib"],
                Some(module),
            )
        };
        let other = |module: &[&str]| {
            placement(
                "other::engine::src",
                &["other::engine::src::lib"],
                Some(module),
            )
        };
        let items = [
            ("crates::engine::src::lib::run", "crates/engine/src/lib.rs"),
            (
                "crates::engine::src::plan::build",
                "crates/engine/src/plan.rs",
            ),
            (
                "crates::engine::src::stray::build",
                "crates/engine/src/stray.rs",
            ),
            (
                "other::engine::src::plan::build",
                "other/engine/src/plan.rs",
            ),
        ];
        let placements = vec![
            ("crates/engine/src/lib.rs", engine(&[])),
            ("crates/engine/src/plan.rs", engine(&["plan"])),
            (
                "crates/engine/src/stray.rs",
                placement("crates::engine::src", &["crates::engine::src::lib"], None),
            ),
            ("other/engine/src/lib.rs", other(&[])),
            ("other/engine/src/plan.rs", other(&["plan"])),
        ];
        let crates = vec![("engine", engine(&[]))];
        with_rust_crates(
            "crates/app/src/main.rs",
            &items,
            placements.clone(),
            crates,
            |ctx| {
                // `engine.run()` has the receiver text of `engine::run()`: a value named like
                // the crate, such as a closure parameter no binding records, never reaches it.
                let at = |receiver: &str, callee: &str| {
                    let value = CallSite {
                        receiver_kind: ReceiverKind::Value,
                        ..module_path_call("scope:worker", receiver, callee)
                    };
                    assert!(
                        matches!(
                            crate::calls::resolve_call_outcome(&value, ctx),
                            ResolutionOutcome::Unresolved { ref candidates, .. }
                                if candidates.is_empty()
                        ),
                        "`{receiver}.{callee}()`"
                    );
                    proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
                };
                assert_eq!(
                    at("engine", "run").as_deref(),
                    Some("crates::engine::src::lib::run")
                );
                assert_eq!(
                    at("engine::plan", "build").as_deref(),
                    Some("crates::engine::src::plan::build")
                );
                assert_eq!(
                    at("engine::stray", "build"),
                    None,
                    "not placed in the crate"
                );
                assert_eq!(at("engine::super", "run"), None);
            },
        );
        // A package that declares no `engine` names no crate by it.
        with_rust_files("crates/nodep/src/lib.rs", &items, placements, |ctx| {
            let call = module_path_call("scope:worker", "engine::plan", "build");
            assert_eq!(proven_target(ctx, &call), None);
            let call = module_path_call("scope:worker", "engine", "run");
            assert_eq!(proven_target(ctx, &call), None);
        });
    }

    #[test]
    fn rust_paths_through_a_type_reach_its_associated_functions() {
        // `crate::a::Engine::new()` in the app and `engine::Engine::new()` through the declared
        // crate `engine`, whose crate root declares its own `Engine`.
        let app = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let engine = |module: &[&str]| {
            placement(
                "crates::engine::src",
                &["crates::engine::src::lib"],
                Some(module),
            )
        };
        let items = [
            ("src::a::Engine (type)", "src/a.rs"),
            ("src::a::new @src::a::Engine", "src/a.rs"),
            ("src::b::Engine (type)", "src/b.rs"),
            ("src::b::new @src::b::Engine", "src/b.rs"),
            ("src::b::new @src::b::Engine2", "src/b.rs"),
            ("src::b::Engine2 (type)", "src/b.rs"),
            (
                "crates::engine::src::lib::Engine (type)",
                "crates/engine/src/lib.rs",
            ),
            (
                "crates::engine::src::lib::new @crates::engine::src::lib::Engine",
                "crates/engine/src/lib.rs",
            ),
        ];
        let placements = vec![
            ("src/lib.rs", app(&[])),
            ("src/a.rs", app(&["a"])),
            ("src/b.rs", app(&["b"])),
            ("crates/engine/src/lib.rs", engine(&[])),
        ];
        let crates = vec![("engine", engine(&[]))];
        with_rust_crates("src/lib.rs", &items, placements, crates, |ctx| {
            let at = |receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
            };
            assert_eq!(
                at("crate::a::Engine", "new").as_deref(),
                Some("src::a::Engine.new")
            );
            assert_eq!(
                at("engine::Engine", "new").as_deref(),
                Some("crates::engine::src::lib::Engine.new")
            );
            // A type the path does not name, and a member the type does not declare.
            assert_eq!(at("crate::a::Missing", "new"), None);
            assert_eq!(at("crate::a::Engine", "build"), None);
            // `src/b.rs` declares `new` on both `Engine` and `Engine2`; the path names `Engine`.
            assert_eq!(
                at("crate::b::Engine", "new").as_deref(),
                Some("src::b::Engine.new")
            );
            let outcome = resolve_module_member_outcome(
                &module_path_call("scope:worker", "crate::a::Engine", "new"),
                ctx,
            );
            let ResolutionOutcome::Proven { candidate } = outcome else {
                panic!("expected a proven edge");
            };
            let kinds = candidate
                .proofs
                .iter()
                .map(|proof| proof.kind)
                .collect::<std::collections::BTreeSet<_>>();
            assert!(kinds.contains(&RelationshipProofKind::ModuleOrPackageBinding));
            assert!(kinds.contains(&RelationshipProofKind::QualifiedName));
            assert!(kinds.contains(&RelationshipProofKind::ContainingType));
        });
    }

    #[test]
    fn rust_path_through_a_type_with_two_same_named_members_is_ambiguous() {
        let app = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        // `impl Engine<u8>` and `impl Engine<u16>` each declare `new`.
        let items = [
            ("src::a::Engine (type)", "src/a.rs"),
            ("src::a::new @src::a::Engine", "src/a.rs"),
        ];
        let placements = vec![("src/lib.rs", app(&[])), ("src/a.rs", app(&["a"]))];
        with_rust_files("src/lib.rs", &items, placements, |ctx| {
            let mut symbols = ctx.symbols.by_id.values().cloned().collect::<Vec<_>>();
            let mut second = ctx
                .symbols
                .get(&SymbolId::new("src::a::Engine.new"))
                .cloned()
                .unwrap();
            second.id = SymbolId::new("src::a::Engine.new#2");
            symbols.push(second);
            let symbols = SymbolIndex::build(symbols);
            let ctx = ResolutionContext::new(
                ctx.file_id,
                ctx.file_path,
                None,
                Language::Rust,
                ctx.repository,
                &symbols,
                ctx.scopes,
                ctx.bindings,
                ctx.inheritance,
                ctx.semantics,
            );
            let call = module_path_call("scope:worker", "crate::a::Engine", "new");
            assert!(matches!(
                resolve_module_member_outcome(&call, &ctx),
                ResolutionOutcome::Ambiguous { .. }
            ));
        });
    }

    #[test]
    fn a_rust_call_path_never_ends_in_a_module_symbol() {
        // `task/mod.rs` declares `mod yield_now;` and re-exports its function, which lives in
        // `task/yield_now.rs`: the module's symbol spells the qualified name the call path does.
        let task = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let items = [
            ("src::task::yield_now (mod)", "src/task/mod.rs"),
            ("src::task::yield_now::yield_now", "src/task/yield_now.rs"),
        ];
        let placements = vec![
            ("src/lib.rs", task(&[])),
            ("src/task/mod.rs", task(&["task"])),
            ("src/task/yield_now.rs", task(&["task", "yield_now"])),
        ];
        with_rust_crates(
            "tests/it.rs",
            &items,
            placements.clone(),
            vec![("engine", task(&[]))],
            |ctx| {
                let call = module_path_call("scope:worker", "engine::task", "yield_now");
                assert_eq!(proven_target(ctx, &call), None);
            },
        );
        with_rust_files("src/lib.rs", &items, placements, |ctx| {
            let call = module_path_call("scope:worker", "crate::task", "yield_now");
            assert_eq!(proven_target(ctx, &call), None);
        });
    }

    #[test]
    fn a_call_through_an_imported_module_never_reaches_a_module_it_declares() {
        // `use engine::task; task::yield_now()`: `task/mod.rs` declares `mod yield_now;`, and
        // the function it re-exports lives in `task/yield_now.rs`.
        let symbol = |id: &str, qualified: &str, kind: SymbolKind, file: &str| Symbol {
            id: SymbolId::new(id),
            name: qualified.rsplit("::").next().unwrap_or_default().into(),
            qualified_name: qualified.into(),
            kind,
            file_id: FileId::new(file),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        };
        let symbols = SymbolIndex::build(vec![
            symbol(
                "module",
                "crates::engine::src::task::yield_now",
                SymbolKind::Module,
                "file:crates/engine/src/task/mod.rs",
            ),
            symbol(
                "function",
                "crates::engine::src::task::yield_now::yield_now",
                SymbolKind::Function,
                "file:crates/engine/src/task/yield_now.rs",
            ),
        ]);
        let caller = FileId::new("file:crates/app/src/lib.rs");
        let mut repository = open_kioku_semantic_model::SemanticRepository::new();
        repository
            .imports
            .insert(open_kioku_semantic_model::ImportBinding {
                file_id: caller.clone(),
                scope_id: ScopeId::new("scope:worker"),
                local_name: "task".into(),
                imported_name: "task".into(),
                source_module: "engine::task".into(),
                resolved_module: None,
                target_file: Some(FileId::new("file:crates/engine/src/task/mod.rs")),
                target_symbol: None,
                origin: open_kioku_semantic_model::ImportOrigin::Internal,
                is_type_only: false,
                is_glob: false,
                evidence: Vec::new(),
                rule: open_kioku_semantic_model::ImportBindingRule::RustModulePath,
                configured_targets: None,
            });
        let scopes = ScopeIndex::build(Vec::new());
        let bindings = BindingIndex::build(Vec::new());
        let inheritance = InheritanceIndex::build(Vec::new());
        let ctx = ResolutionContext::new(
            &caller,
            std::path::Path::new("crates/app/src/lib.rs"),
            None,
            Language::Rust,
            &repository,
            &symbols,
            &scopes,
            &bindings,
            &inheritance,
            open_kioku_languages::semantics_for(&Language::Rust).unwrap(),
        );
        let call = CallSite {
            file_id: caller.clone(),
            receiver_kind: ReceiverKind::Value,
            ..module_path_call("scope:worker", "task", "yield_now")
        };
        assert!(matches!(
            imported_receiver_outcome(&call, &ctx, "task"),
            ResolutionOutcome::Unresolved { ref candidates, .. } if candidates.is_empty()
        ));
    }

    #[test]
    fn rust_module_paths_are_not_read_from_a_file_another_crate_may_compile() {
        // `main.rs` declares `util`, and `lib.rs`, which the index could not read, may too:
        // `crate::helper` in `util.rs` would be the library's `helper` in that crate (#572).
        let bin = |module: &[&str]| placement("src", &["src::main"], Some(module));
        // `main.rs` mounts it with `#[path]`, say, where its `mod` items are read elsewhere.
        let shared = RustModulePlacement {
            in_other_crates: true,
            own_subtree_in_every_crate: false,
            ..bin(&["util"])
        };
        let items = [
            ("src::main::helper", "src/main.rs"),
            ("src::util::u", "src/util.rs"),
            ("src::util::inner::f", "src/util/inner.rs"),
        ];
        let placements = vec![
            ("src/main.rs", bin(&[])),
            ("src/util.rs", shared.clone()),
            ("src/util/inner.rs", bin(&["util", "inner"])),
        ];
        with_rust_files("src/util.rs", &items, placements.clone(), |ctx| {
            let at = |receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
            };
            assert_eq!(at("crate", "helper"), None);
            assert_eq!(at("self::inner", "f"), None);
        });
        // The binary's own paths still end in it.
        with_rust_files("src/main.rs", &items, placements.clone(), |ctx| {
            let call = module_path_call("scope:worker", "crate::util", "u");
            assert_eq!(proven_target(ctx, &call).as_deref(), Some("src::util::u"));
        });

        // Where every crate declares `util` at the same place, `util/inner.rs` is the same file in
        // each, so a path that ends below `util` is read; one that leaves it is not (#576).
        let mut in_place = placements;
        in_place[1].1.own_subtree_in_every_crate = true;
        with_rust_files("src/util.rs", &items, in_place, |ctx| {
            let at = |receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
            };
            assert_eq!(
                at("self::inner", "f").as_deref(),
                Some("src::util::inner::f")
            );
            assert_eq!(at("crate", "helper"), None);
            assert_eq!(at("super", "helper"), None);
            assert_eq!(at("crate::util::inner", "f"), None);
        });
    }

    #[test]
    fn rust_crate_paths_stay_in_the_crate_root_whose_tree_holds_the_caller() {
        // `main.rs` declares `cli`, `lib.rs` declares `core`: two crates in one `src/`.
        let bin = |module: &[&str]| placement("src", &["src::main"], Some(module));
        let lib = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let items = [
            ("src::main::helper", "src/main.rs"),
            ("src::lib::helper", "src/lib.rs"),
            ("src::core::k", "src/core.rs"),
            ("src::cli::run", "src/cli.rs"),
        ];
        let placements = vec![
            ("src/main.rs", bin(&[])),
            ("src/cli.rs", bin(&["cli"])),
            ("src/lib.rs", lib(&[])),
            ("src/core.rs", lib(&["core"])),
        ];
        with_rust_files("src/cli.rs", &items, placements.clone(), |ctx| {
            let at = |receiver: &str, callee: &str| {
                proven_target(ctx, &module_path_call("scope:worker", receiver, callee))
            };
            assert_eq!(at("crate", "helper").as_deref(), Some("src::main::helper"));
            assert_eq!(at("super", "helper").as_deref(), Some("src::main::helper"));
            assert_eq!(at("crate::core", "k"), None);
        });
        with_rust_files("src/main.rs", &items, placements, |ctx| {
            let call = module_path_call("scope:worker", "crate::cli", "run");
            assert_eq!(proven_target(ctx, &call).as_deref(), Some("src::cli::run"));
        });
    }

    #[test]
    fn rust_raw_identifier_module_paths_reach_the_module_file() {
        let lib = |module: &[&str]| placement("src", &["src::lib"], Some(module));
        let items = [("src::type::ty", "src/type.rs")];
        let placements = vec![("src/lib.rs", lib(&[])), ("src/type.rs", lib(&["type"]))];
        with_rust_files("src/lib.rs", &items, placements, |ctx| {
            let call = module_path_call("scope:worker", "crate::r#type", "ty");
            assert_eq!(proven_target(ctx, &call).as_deref(), Some("src::type::ty"));
        });
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
