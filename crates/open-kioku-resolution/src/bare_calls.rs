use crate::context::{ResolutionContext, ScopedImport};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::pipeline::{evaluate_candidates, ResolutionCandidate, ResolutionOutcome};
use open_kioku_core::{
    CallSite, Confidence, EvidenceSourceType, FileRange, GraphEdgeType, Language, LineRange,
    RelationshipProof, RelationshipProofKind, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::ImportBindingRule;

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

    // The nearest import of the name in scope decides. An unresolved one, or a glob in a nearer
    // scope, leaves the import rule without a candidate: the unresolved import may be the real
    // target, and a resolved import further out is shadowed.
    if let ScopedImport::Resolved(bindings) =
        ctx.scoped_import(&call.scope_id, &call.callee_name, |binding| {
            binding.target_symbol.is_some()
        })
    {
        let mut imported_targets = bindings
            .iter()
            .filter_map(|binding| binding.target_symbol.clone())
            .collect::<Vec<_>>();
        if ctx.language == Language::Rust {
            // A Rust `use` also binds tuple structs and constants by name; as for same-scope and
            // same-file calls, only a function is a call target.
            imported_targets.retain(|target| {
                ctx.symbols
                    .get(target)
                    .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
            });
        }
        let (strategy, message) = if bindings
            .iter()
            .all(|binding| binding.rule == ImportBindingRule::RustModulePath)
        {
            (
                "rust_item_import",
                "bare call candidate from a Rust item import bound by its module path",
            )
        } else {
            (
                "explicit_import",
                "bare call candidate from exact import binding",
            )
        };
        normalize_symbol_ids(&mut imported_targets);
        if !imported_targets.is_empty() {
            return evaluate_target_set(
                call,
                ctx,
                imported_targets,
                Confidence::Exact,
                ResolutionEvidenceKind::ExplicitImport,
                strategy,
                message,
                &[
                    RelationshipProofKind::ImportBinding,
                    RelationshipProofKind::QualifiedName,
                ],
            );
        }
    }

    if ctx.language != Language::Java {
        let mut same_file_candidates = ctx
            .symbols
            .lookup_file_name(ctx.file_id, &call.callee_name)
            .iter()
            .filter(|id| {
                ctx.symbols
                    .get(id)
                    .map(|symbol| {
                        symbol.parent_symbol_id.is_none()
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
    crate::context::nearest_lexical_items(ctx, &call.scope_id, &call.callee_name, |symbol| {
        matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
    })
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
        Binding, BindingId, CallSiteId, FileId, Language, ReceiverKind, RelationshipAuthority,
        Scope, ScopeId, ScopeKind, SourceRange, Symbol, SymbolKind, Visibility,
    };
    use open_kioku_semantic_model::{ImportBinding, ImportOrigin, GLOB_IMPORT_LOCAL_NAME};
    use std::collections::BTreeSet;

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
        with_resolution_context(
            symbols,
            bindings,
            Vec::new(),
            vec![scope("scope:file", None, ScopeKind::File)],
            test,
        )
    }

    fn scope(id: &str, parent: Option<&str>, kind: ScopeKind) -> Scope {
        Scope {
            id: ScopeId::new(id),
            file_id: FileId::new("file:src/lib.rs"),
            parent_id: parent.map(ScopeId::new),
            owner_symbol_id: None,
            kind,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 50,
                end_column: 1,
            },
        }
    }

    fn with_resolution_context<T>(
        symbols: Vec<Symbol>,
        bindings: Vec<Binding>,
        imports: Vec<ImportBinding>,
        scopes: Vec<Scope>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        with_language_resolution_context(Language::Rust, symbols, bindings, imports, scopes, test)
    }

    fn with_language_resolution_context<T>(
        language: Language,
        symbols: Vec<Symbol>,
        bindings: Vec<Binding>,
        imports: Vec<ImportBinding>,
        scopes: Vec<Scope>,
        test: impl FnOnce(&ResolutionContext<'_>) -> T,
    ) -> T {
        let file_id = FileId::new("file:src/lib.rs");
        let scopes = ScopeIndex::build(scopes);
        let symbol_index = SymbolIndex::build(symbols);
        let bindings = BindingIndex::build(bindings);
        let inheritance = InheritanceIndex::build(Vec::new());
        let mut repository = open_kioku_semantic_model::SemanticRepository::new();
        for import in imports {
            repository.imports.insert(import);
        }
        let semantics = open_kioku_languages::semantics_for(&language).unwrap();
        let context = ResolutionContext::new(
            &file_id,
            std::path::Path::new("src/lib.rs"),
            None,
            language,
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

    fn import_binding(
        scope_id: &str,
        local: &str,
        source: &str,
        target: Option<&str>,
    ) -> ImportBinding {
        ImportBinding {
            file_id: FileId::new("file:src/lib.rs"),
            scope_id: ScopeId::new(scope_id),
            local_name: local.into(),
            imported_name: source.rsplit("::").next().unwrap_or(source).into(),
            source_module: source.into(),
            resolved_module: None,
            target_file: None,
            target_symbol: target.map(SymbolId::new),
            origin: if target.is_some() {
                ImportOrigin::Internal
            } else {
                ImportOrigin::Unknown
            },
            is_type_only: false,
            is_glob: false,
            evidence: Vec::new(),
            rule: if target.is_some() {
                ImportBindingRule::RustModulePath
            } else {
                ImportBindingRule::ModuleKey
            },
        }
    }

    fn glob_import(scope_id: &str, source: &str) -> ImportBinding {
        ImportBinding {
            local_name: GLOB_IMPORT_LOCAL_NAME.into(),
            imported_name: GLOB_IMPORT_LOCAL_NAME.into(),
            is_glob: true,
            ..import_binding(scope_id, GLOB_IMPORT_LOCAL_NAME, source, None)
        }
    }

    fn call_in(scope_id: &str, callee: &str) -> CallSite {
        CallSite {
            scope_id: ScopeId::new(scope_id),
            callee_name: callee.into(),
            ..bare_call()
        }
    }

    /// `use tokio::spawn;` (optional) at file scope, `pub fn run() { spawn(..) }`, and
    /// `mod tests { use crate::testing::spawn; fn t() { spawn(..) } }`.
    fn sibling_scope_layout(with_unresolved_file_import: bool) -> (Vec<Scope>, Vec<ImportBinding>) {
        let scopes = vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:run", Some("scope:file"), ScopeKind::Function),
            scope("scope:tests", Some("scope:file"), ScopeKind::Module),
            scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
        ];
        let mut imports = vec![import_binding(
            "scope:tests",
            "spawn",
            "crate::testing::spawn",
            Some("symbol:spawn"),
        )];
        if with_unresolved_file_import {
            imports.push(import_binding("scope:file", "spawn", "tokio::spawn", None));
        }
        (scopes, imports)
    }

    #[test]
    fn import_in_a_sibling_scope_does_not_answer_for_a_call() {
        let spawn = || vec![symbol("symbol:spawn", "spawn", "file:src/testing.rs", None)];
        let (scopes, imports) = sibling_scope_layout(false);
        with_resolution_context(spawn(), Vec::new(), imports, scopes, |ctx| {
            let production = resolve_bare_call_outcome(&call_in("scope:run", "spawn"), ctx);
            assert!(
                !matches!(production, ResolutionOutcome::Proven { .. }),
                "the tests module's import is not in scope in `run`: {production:?}"
            );
            let in_tests = resolve_bare_call_outcome(&call_in("scope:tests:t", "spawn"), ctx);
            assert!(
                matches!(in_tests, ResolutionOutcome::Proven { .. }),
                "the tests module's own call binds through its import: {in_tests:?}"
            );
        });

        let (scopes, imports) = sibling_scope_layout(true);
        with_resolution_context(spawn(), Vec::new(), imports, scopes, |ctx| {
            let production = resolve_bare_call_outcome(&call_in("scope:run", "spawn"), ctx);
            assert!(
                !matches!(production, ResolutionOutcome::Proven { .. }),
                "the unresolved `tokio::spawn` is the import in scope in `run`: {production:?}"
            );
            // A Rust `mod` block does not see the file's `use tokio::spawn;`.
            let in_tests = resolve_bare_call_outcome(&call_in("scope:tests:t", "spawn"), ctx);
            assert!(
                matches!(in_tests, ResolutionOutcome::Proven { .. }),
                "the tests module's call binds through its own import: {in_tests:?}"
            );
        });
    }

    #[test]
    fn rust_mod_block_does_not_see_the_enclosing_files_imports() {
        // `use crate::auth::spawn;` at file level, `mod tests { [use crate::fakes::*;] fn t() }`.
        let scopes = || {
            vec![
                scope("scope:file", None, ScopeKind::File),
                scope("scope:run", Some("scope:file"), ScopeKind::Function),
                scope("scope:tests", Some("scope:file"), ScopeKind::Module),
                scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
            ]
        };
        let file_import = || {
            import_binding(
                "scope:file",
                "spawn",
                "crate::auth::spawn",
                Some("symbol:spawn"),
            )
        };
        let spawn = || vec![symbol("symbol:spawn", "spawn", "file:src/auth.rs", None)];

        for imports in [
            vec![file_import()],
            vec![file_import(), glob_import("scope:tests", "crate::fakes::*")],
        ] {
            with_resolution_context(spawn(), Vec::new(), imports, scopes(), |ctx| {
                let in_run = resolve_bare_call_outcome(&call_in("scope:run", "spawn"), ctx);
                assert!(
                    matches!(in_run, ResolutionOutcome::Proven { .. }),
                    "{in_run:?}"
                );
                let in_tests = resolve_bare_call_outcome(&call_in("scope:tests:t", "spawn"), ctx);
                assert!(
                    !matches!(in_tests, ResolutionOutcome::Proven { .. }),
                    "the file-level import is not in scope inside `mod tests`: {in_tests:?}"
                );
            });
        }
    }

    #[test]
    fn nearest_import_decides_and_a_nearer_glob_leaves_the_name_unresolved() {
        // `use crate::a::f;` at file level; inside `fn run`, a block with its own import.
        let scopes = || {
            vec![
                scope("scope:file", None, ScopeKind::File),
                scope("scope:run", Some("scope:file"), ScopeKind::Function),
                scope("scope:block", Some("scope:run"), ScopeKind::Block),
            ]
        };
        let symbols = || {
            vec![
                symbol("symbol:a:f", "f", "file:src/a.rs", None),
                symbol("symbol:b:f", "f", "file:src/b.rs", None),
            ]
        };
        let file_import = || import_binding("scope:file", "f", "crate::a::f", Some("symbol:a:f"));
        let proven_target = |outcome: ResolutionOutcome| match outcome {
            ResolutionOutcome::Proven { candidate } => Some(candidate.target_symbol_id.0),
            _ => None,
        };

        let cases = [
            (
                "a resolved block import shadows the file's",
                vec![
                    file_import(),
                    import_binding("scope:block", "f", "crate::b::f", Some("symbol:b:f")),
                ],
                Some("symbol:b:f"),
            ),
            (
                "an unresolved block import does not fall back to the file's",
                vec![
                    file_import(),
                    import_binding("scope:block", "f", "crate::b::f", None),
                ],
                None,
            ),
            (
                "a block glob may supply the name",
                vec![file_import(), glob_import("scope:block", "crate::b::*")],
                None,
            ),
        ];
        for (layout, imports, expected) in cases {
            with_resolution_context(symbols(), Vec::new(), imports, scopes(), |ctx| {
                assert_eq!(
                    proven_target(resolve_bare_call_outcome(&call_in("scope:block", "f"), ctx))
                        .as_deref(),
                    expected,
                    "{layout}"
                );
                assert_eq!(
                    proven_target(resolve_bare_call_outcome(&call_in("scope:run", "f"), ctx))
                        .as_deref(),
                    Some("symbol:a:f"),
                    "{layout}: outside the block the file's import decides"
                );
            });
        }
    }

    #[test]
    fn rust_import_bound_to_a_non_function_is_not_a_call_target() {
        // `use crate::auth::Token as run;` then `run(..)`: a tuple struct, not a function.
        with_resolution_context(
            vec![Symbol {
                kind: SymbolKind::Class,
                ..symbol("symbol:Token", "Token", "file:src/auth.rs", None)
            }],
            Vec::new(),
            vec![import_binding(
                "global",
                "run",
                "crate::auth::Token",
                Some("symbol:Token"),
            )],
            vec![scope("scope:file", None, ScopeKind::File)],
            |ctx| {
                let outcome = resolve_bare_call_outcome(&bare_call(), ctx);
                assert!(
                    !matches!(outcome, ResolutionOutcome::Proven { .. }),
                    "{outcome:?}"
                );
            },
        );
    }

    #[test]
    fn type_import_is_a_candidate_only_where_it_is_in_scope_and_unopposed() {
        // `mod tests { use crate::fake::Clock; }`, optionally beside `use real::Clock;`.
        let fake_clock = || {
            vec![Symbol {
                kind: SymbolKind::Class,
                ..symbol("symbol:FakeClock", "Clock", "file:src/fake.rs", None)
            }]
        };
        let candidates = |ctx: &ResolutionContext<'_>, scope_id: &str| {
            crate::typed_calls::collect_type_candidates(ctx, &ScopeId::new(scope_id), "Clock")
        };
        let rebind = |imports: Vec<ImportBinding>| {
            imports
                .into_iter()
                .map(|binding| ImportBinding {
                    local_name: "Clock".into(),
                    imported_name: "Clock".into(),
                    source_module: binding.source_module.replace("spawn", "Clock"),
                    target_symbol: binding
                        .target_symbol
                        .map(|_| SymbolId::new("symbol:FakeClock")),
                    ..binding
                })
                .collect::<Vec<_>>()
        };

        let (scopes, imports) = sibling_scope_layout(false);
        with_resolution_context(fake_clock(), Vec::new(), rebind(imports), scopes, |ctx| {
            assert!(candidates(ctx, "scope:run").is_empty());
            assert_eq!(
                candidates(ctx, "scope:tests:t"),
                vec![SymbolId::new("symbol:FakeClock")]
            );
        });

        // A Rust `mod` block does not see the file's unresolved `use real::Clock;`.
        let (scopes, imports) = sibling_scope_layout(true);
        with_resolution_context(fake_clock(), Vec::new(), rebind(imports), scopes, |ctx| {
            assert_eq!(
                candidates(ctx, "scope:tests:t"),
                vec![SymbolId::new("symbol:FakeClock")]
            );
            assert!(candidates(ctx, "scope:run").is_empty());
        });

        // A glob in a nearer scope may supply the name instead.
        let (scopes, imports) = sibling_scope_layout(false);
        let mut imports = rebind(imports);
        imports.push(glob_import("scope:tests:t", "crate::fakes::*"));
        with_resolution_context(fake_clock(), Vec::new(), imports, scopes, |ctx| {
            assert!(candidates(ctx, "scope:tests:t").is_empty());
        });
    }

    fn proven_bare_call_target(
        ctx: &ResolutionContext<'_>,
        scope_id: &str,
        callee: &str,
    ) -> Option<String> {
        match resolve_bare_call_outcome(&call_in(scope_id, callee), ctx) {
            ResolutionOutcome::Proven { candidate } => Some(candidate.target_symbol_id.0),
            _ => None,
        }
    }

    /// `from <module> import <name>` recorded at `scope_id` and bound to `symbol:<module>:<name>`.
    fn python_import(scope_id: &str, module: &str, name: &str) -> ImportBinding {
        ImportBinding {
            imported_name: name.into(),
            rule: ImportBindingRule::ModuleKey,
            ..import_binding(
                scope_id,
                name,
                module,
                Some(&format!("symbol:{module}:{name}")),
            )
        }
    }

    /// A Python module with `if TYPE_CHECKING:`, `try:` and `except ImportError:` bodies, and
    /// `def load():` and `def helper():`, whose body has an `if` body of its own.
    fn python_block_layout() -> Vec<Scope> {
        vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:if", Some("scope:file"), ScopeKind::Block),
            scope("scope:try", Some("scope:file"), ScopeKind::Block),
            scope("scope:except", Some("scope:file"), ScopeKind::Block),
            scope("scope:load", Some("scope:file"), ScopeKind::Function),
            scope("scope:load:body", Some("scope:load"), ScopeKind::Block),
            scope("scope:helper", Some("scope:file"), ScopeKind::Function),
            scope("scope:helper:body", Some("scope:helper"), ScopeKind::Block),
            scope(
                "scope:helper:if",
                Some("scope:helper:body"),
                ScopeKind::Block,
            ),
        ]
    }

    #[test]
    fn python_import_in_a_block_binds_in_the_enclosing_function_or_module() {
        let symbols = || {
            vec![
                symbol("symbol:fast:encode", "encode", "file:fast.py", None),
                symbol("symbol:slow:encode", "encode", "file:slow.py", None),
            ]
        };
        let python = |imports: Vec<ImportBinding>, test: &dyn Fn(&ResolutionContext<'_>)| {
            with_language_resolution_context(
                Language::Python,
                symbols(),
                Vec::new(),
                imports,
                python_block_layout(),
                test,
            )
        };

        // `try: from fast import encode` at module level binds in the module.
        python(vec![python_import("scope:try", "fast", "encode")], &|ctx| {
            assert_eq!(
                proven_bare_call_target(ctx, "scope:load:body", "encode").as_deref(),
                Some("symbol:fast:encode")
            );
        });

        // The same import in an `if` body inside `def helper():` binds in `helper` only.
        python(
            vec![python_import("scope:helper:if", "fast", "encode")],
            &|ctx| {
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:helper:body", "encode").as_deref(),
                    Some("symbol:fast:encode")
                );
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:load:body", "encode"),
                    None
                );
            },
        );

        // `try: from fast import encode` / `except ImportError: from slow import encode`: both
        // bind the name in the module, so both stay candidates and neither is proven.
        python(
            vec![
                python_import("scope:try", "fast", "encode"),
                python_import("scope:except", "slow", "encode"),
            ],
            &|ctx| match resolve_bare_call_outcome(&call_in("scope:load:body", "encode"), ctx) {
                ResolutionOutcome::Ambiguous { candidates, .. }
                | ResolutionOutcome::Unresolved { candidates, .. } => {
                    let targets = candidates
                        .into_iter()
                        .map(|candidate| candidate.target_symbol_id.0)
                        .collect::<BTreeSet<_>>();
                    assert_eq!(
                        targets,
                        BTreeSet::from([
                            "symbol:fast:encode".to_string(),
                            "symbol:slow:encode".to_string(),
                        ])
                    );
                }
                other => panic!("two imports of the name must not prove one target: {other:?}"),
            },
        );
    }

    #[test]
    fn python_class_body_import_is_not_visible_in_its_methods() {
        // `from a import g` at module level, and `class C:` whose body has `from b import g` and
        // `def m(self): g()`.
        let scopes = vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:C", Some("scope:file"), ScopeKind::Class),
            scope("scope:C:body", Some("scope:C"), ScopeKind::Block),
            scope("scope:m", Some("scope:C:body"), ScopeKind::Function),
            scope("scope:m:body", Some("scope:m"), ScopeKind::Block),
        ];
        with_language_resolution_context(
            Language::Python,
            vec![
                symbol("symbol:a:g", "g", "file:a.py", None),
                symbol("symbol:b:g", "g", "file:b.py", None),
            ],
            Vec::new(),
            vec![
                python_import("scope:file", "a", "g"),
                python_import("scope:C:body", "b", "g"),
            ],
            scopes,
            |ctx| {
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:m:body", "g").as_deref(),
                    Some("symbol:a:g"),
                    "a method body resolves free names in the module, not the class body"
                );
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:C:body", "g").as_deref(),
                    Some("symbol:b:g"),
                    "the class body sees its own import"
                );
            },
        );
    }

    #[test]
    fn python_type_imported_under_type_checking_is_a_candidate_in_module_level_code() {
        // `if TYPE_CHECKING: from app.models import User`, then `def load(u: User)` and
        // `class Admin(User)`.
        let user = SymbolId::new("symbol:app.models:User");
        with_language_resolution_context(
            Language::Python,
            vec![Symbol {
                kind: SymbolKind::Class,
                ..symbol(&user.0, "User", "file:app/models.py", None)
            }],
            Vec::new(),
            vec![python_import("scope:if", "app.models", "User")],
            python_block_layout(),
            |ctx| {
                assert_eq!(
                    crate::typed_calls::collect_type_candidate_origins(
                        ctx,
                        &ScopeId::new("scope:load"),
                        "User"
                    ),
                    vec![(user.clone(), true)]
                );
                let admin = Symbol {
                    kind: SymbolKind::Class,
                    language: Language::Python,
                    ..symbol(
                        "symbol:Admin",
                        "Admin",
                        "file:src/lib.rs",
                        Some("scope:file"),
                    )
                };
                let parents = crate::type_relations::collect_parent_type_candidates(
                    &admin,
                    "User",
                    ctx.symbols,
                    ctx.repository,
                    Some(ctx.scopes),
                );
                assert_eq!(
                    parents
                        .iter()
                        .map(|candidate| candidate.target.clone())
                        .collect::<Vec<_>>(),
                    vec![user.clone()]
                );
            },
        );
    }

    #[test]
    fn super_glob_in_a_mod_block_continues_into_the_parent_module() {
        // `use crate::auth::spawn;` at file level; `mod tests { use super::*; fn t() { spawn() } }`.
        let scopes = || {
            vec![
                scope("scope:file", None, ScopeKind::File),
                scope("scope:tests", Some("scope:file"), ScopeKind::Module),
                scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
            ]
        };
        let symbols = || {
            vec![
                symbol("symbol:spawn", "spawn", "file:src/auth.rs", None),
                symbol("symbol:fake_spawn", "spawn", "file:src/fakes.rs", None),
            ]
        };
        let file_import = || {
            import_binding(
                "scope:file",
                "spawn",
                "crate::auth::spawn",
                Some("symbol:spawn"),
            )
        };
        let super_glob = || glob_import("scope:tests", "super::*");
        let cases = [
            (
                "`use super::*;` reaches the file's import",
                vec![file_import(), super_glob()],
                Some("symbol:spawn"),
            ),
            (
                "a second glob may supply the name",
                vec![
                    file_import(),
                    super_glob(),
                    glob_import("scope:tests", "crate::fakes::*"),
                ],
                None,
            ),
            (
                "an explicit import in the module shadows the glob",
                vec![
                    file_import(),
                    super_glob(),
                    import_binding(
                        "scope:tests",
                        "spawn",
                        "crate::fakes::spawn",
                        Some("symbol:fake_spawn"),
                    ),
                ],
                Some("symbol:fake_spawn"),
            ),
            (
                "an unresolved explicit import in the module is not bypassed",
                vec![
                    file_import(),
                    super_glob(),
                    import_binding("scope:tests", "spawn", "tokio::spawn", None),
                ],
                None,
            ),
            (
                "a glob of another module does not continue into the parent",
                vec![
                    file_import(),
                    glob_import("scope:tests", "self::helpers::*"),
                ],
                None,
            ),
        ];
        for (layout, imports, expected) in cases {
            with_resolution_context(symbols(), Vec::new(), imports, scopes(), |ctx| {
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:tests:t", "spawn").as_deref(),
                    expected,
                    "{layout}"
                );
            });
        }
    }

    #[test]
    fn super_super_glob_climbs_two_modules_and_never_past_the_file() {
        let file_import = || {
            import_binding(
                "scope:file",
                "spawn",
                "crate::auth::spawn",
                Some("symbol:spawn"),
            )
        };
        let symbols = || vec![symbol("symbol:spawn", "spawn", "file:src/auth.rs", None)];
        // `mod outer { mod inner { use super::super::*; fn t() { spawn() } } }`.
        let nested = || {
            vec![
                scope("scope:file", None, ScopeKind::File),
                scope("scope:outer", Some("scope:file"), ScopeKind::Module),
                scope("scope:inner", Some("scope:outer"), ScopeKind::Module),
                scope("scope:inner:t", Some("scope:inner"), ScopeKind::Function),
            ]
        };
        with_resolution_context(
            symbols(),
            Vec::new(),
            vec![file_import(), glob_import("scope:inner", "super::super::*")],
            nested(),
            |ctx| {
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:inner:t", "spawn").as_deref(),
                    Some("symbol:spawn")
                );
            },
        );
        // `use super::*;` in `inner` names `outer`, which imports nothing.
        with_resolution_context(
            symbols(),
            Vec::new(),
            vec![file_import(), glob_import("scope:inner", "super::*")],
            nested(),
            |ctx| assert_eq!(proven_bare_call_target(ctx, "scope:inner:t", "spawn"), None),
        );
        // From a module directly in the file, `super::super` is outside the file.
        with_resolution_context(
            symbols(),
            Vec::new(),
            vec![file_import(), glob_import("scope:tests", "super::super::*")],
            vec![
                scope("scope:file", None, ScopeKind::File),
                scope("scope:tests", Some("scope:file"), ScopeKind::Module),
                scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
            ],
            |ctx| assert_eq!(proven_bare_call_target(ctx, "scope:tests:t", "spawn"), None),
        );
    }

    /// `fn now() {}` at file level; `mod tests { [imports] fn helper() {} fn t() { .. } }`; and a
    /// sibling `mod other { fn t() { .. } }`.
    fn mod_block_layout() -> Vec<Scope> {
        vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:tests", Some("scope:file"), ScopeKind::Module),
            scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
            scope(
                "scope:tests:t:body",
                Some("scope:tests:t"),
                ScopeKind::Block,
            ),
            scope("scope:other", Some("scope:file"), ScopeKind::Module),
            scope("scope:other:t", Some("scope:other"), ScopeKind::Function),
        ]
    }

    fn mod_block_symbols() -> Vec<Symbol> {
        vec![
            symbol("symbol:now", "now", "file:src/lib.rs", Some("scope:file")),
            symbol(
                "symbol:tests:helper",
                "helper",
                "file:src/lib.rs",
                Some("scope:tests"),
            ),
            symbol("symbol:clock:now", "now", "file:src/clock.rs", None),
        ]
    }

    #[test]
    fn rust_lexical_lookup_stops_at_the_enclosing_mod_block() {
        let proven = |imports: Vec<ImportBinding>, scope_id: &str, callee: &str| {
            with_resolution_context(
                mod_block_symbols(),
                Vec::new(),
                imports,
                mod_block_layout(),
                |ctx| proven_bare_call_target(ctx, scope_id, callee),
            )
        };
        let cases = [
            (
                "`use crate::clock::now;` in the module names the imported item",
                vec![import_binding(
                    "scope:tests",
                    "now",
                    "crate::clock::now",
                    Some("symbol:clock:now"),
                )],
                "scope:tests:t:body",
                "now",
                Some("symbol:clock:now"),
            ),
            (
                "an unresolved import in the module is not replaced by the file's item",
                vec![import_binding(
                    "scope:tests",
                    "now",
                    "crate::clock::now",
                    None,
                )],
                "scope:tests:t:body",
                "now",
                None,
            ),
            (
                "the file's item is not in scope inside the module without an import",
                Vec::new(),
                "scope:tests:t:body",
                "now",
                None,
            ),
            (
                "`use super::*;` brings the file's item into the module",
                vec![glob_import("scope:tests", "super::*")],
                "scope:tests:t:body",
                "now",
                Some("symbol:now"),
            ),
            (
                "an explicit import in the module shadows the item `use super::*;` reaches",
                vec![
                    glob_import("scope:tests", "super::*"),
                    import_binding(
                        "scope:tests",
                        "now",
                        "crate::clock::now",
                        Some("symbol:clock:now"),
                    ),
                ],
                "scope:tests:t:body",
                "now",
                Some("symbol:clock:now"),
            ),
            (
                "a second glob beside `use super::*;` may supply the name",
                vec![
                    glob_import("scope:tests", "super::*"),
                    glob_import("scope:tests", "crate::fakes::*"),
                ],
                "scope:tests:t:body",
                "now",
                None,
            ),
            (
                "`use super::now;` names the file's item",
                vec![import_binding("scope:tests", "now", "super::now", None)],
                "scope:tests:t:body",
                "now",
                Some("symbol:now"),
            ),
            (
                "`use super::now as tick;` names it under the alias",
                vec![import_binding("scope:tests", "tick", "super::now", None)],
                "scope:tests:t:body",
                "tick",
                Some("symbol:now"),
            ),
            (
                "`use super::helpers::now;` names an item of another module",
                vec![import_binding(
                    "scope:tests",
                    "now",
                    "super::helpers::now",
                    None,
                )],
                "scope:tests:t:body",
                "now",
                None,
            ),
            (
                "`use super::super::now;` climbs past the file",
                vec![import_binding(
                    "scope:tests",
                    "now",
                    "super::super::now",
                    None,
                )],
                "scope:tests:t:body",
                "now",
                None,
            ),
            (
                "an item of the module itself is in scope",
                Vec::new(),
                "scope:tests:t:body",
                "helper",
                Some("symbol:tests:helper"),
            ),
            (
                "an item of a sibling module is not",
                Vec::new(),
                "scope:other:t",
                "helper",
                None,
            ),
        ];
        for (layout, imports, scope_id, callee, expected) in cases {
            assert_eq!(
                proven(imports, scope_id, callee).as_deref(),
                expected,
                "{layout}"
            );
        }
    }

    #[test]
    fn rust_import_in_a_nearer_block_shadows_an_item_of_the_module() {
        // `fn now() {}` at file level; `fn t() { use crate::clock::now; now() }`.
        let scopes = vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:t", Some("scope:file"), ScopeKind::Function),
            scope("scope:t:body", Some("scope:t"), ScopeKind::Block),
        ];
        for (imports, expected) in [
            (
                vec![import_binding(
                    "scope:t:body",
                    "now",
                    "crate::clock::now",
                    Some("symbol:clock:now"),
                )],
                Some("symbol:clock:now"),
            ),
            (vec![glob_import("scope:t:body", "crate::clock::*")], None),
            (Vec::new(), Some("symbol:now")),
        ] {
            with_resolution_context(
                mod_block_symbols(),
                Vec::new(),
                imports.clone(),
                scopes.clone(),
                |ctx| {
                    assert_eq!(
                        proven_bare_call_target(ctx, "scope:t:body", "now").as_deref(),
                        expected,
                        "{imports:?}"
                    );
                },
            );
        }
    }

    #[test]
    fn rust_associated_function_is_not_in_lexical_scope() {
        // `fn helper() {}` at file level; `impl Parser { fn helper(&self) {} fn run(&self) {
        // helper() } }`: the bare call names the free function.
        let scopes = vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:impl", Some("scope:file"), ScopeKind::Trait),
            scope("scope:impl:run", Some("scope:impl"), ScopeKind::Function),
        ];
        let method = Symbol {
            kind: SymbolKind::Method,
            ..symbol(
                "symbol:Parser:helper",
                "helper",
                "file:src/lib.rs",
                Some("scope:impl"),
            )
        };
        let free = symbol(
            "symbol:helper",
            "helper",
            "file:src/lib.rs",
            Some("scope:file"),
        );
        with_resolution_context(
            vec![method.clone(), free],
            Vec::new(),
            Vec::new(),
            scopes.clone(),
            |ctx| {
                assert_eq!(
                    proven_bare_call_target(ctx, "scope:impl:run", "helper").as_deref(),
                    Some("symbol:helper")
                );
            },
        );
        with_resolution_context(vec![method], Vec::new(), Vec::new(), scopes, |ctx| {
            assert_eq!(
                proven_bare_call_target(ctx, "scope:impl:run", "helper"),
                None
            );
        });
    }

    #[test]
    fn rust_same_file_type_is_a_candidate_only_from_its_own_module() {
        // `mod fakes { pub struct Client; }`, `use reqwest::Client;` (optional) and `fn run()` at
        // file level; `mod tests { [use super::*;] fn t() }`; and `struct Local` in `tests`.
        let scopes = vec![
            scope("scope:file", None, ScopeKind::File),
            scope("scope:fakes", Some("scope:file"), ScopeKind::Module),
            scope("scope:run", Some("scope:file"), ScopeKind::Function),
            scope("scope:tests", Some("scope:file"), ScopeKind::Module),
            scope("scope:tests:t", Some("scope:tests"), ScopeKind::Function),
        ];
        let class = |id: &str, name: &str, scope_id: &str| Symbol {
            kind: SymbolKind::Class,
            ..symbol(id, name, "file:src/lib.rs", Some(scope_id))
        };
        let symbols = || {
            vec![
                class("symbol:fakes:Client", "Client", "scope:fakes"),
                class("symbol:Config", "Config", "scope:file"),
                class("symbol:tests:Local", "Local", "scope:tests"),
            ]
        };
        let reqwest = || import_binding("scope:file", "Client", "reqwest::Client", None);
        let candidates = |imports: Vec<ImportBinding>, scope_id: &str, name: &str| {
            with_resolution_context(symbols(), Vec::new(), imports, scopes.clone(), |ctx| {
                crate::typed_calls::collect_type_candidates(ctx, &ScopeId::new(scope_id), name)
            })
        };
        let ids = |ids: &[&str]| ids.iter().map(|id| SymbolId::new(*id)).collect::<Vec<_>>();

        assert_eq!(
            candidates(vec![reqwest()], "scope:run", "Client"),
            ids(&[]),
            "`Client` in `run` names the unresolved import, not `fakes::Client`"
        );
        assert_eq!(
            candidates(Vec::new(), "scope:run", "Client"),
            ids(&[]),
            "a type in a sibling module needs a path or an import"
        );
        assert_eq!(
            candidates(Vec::new(), "scope:run", "Config"),
            ids(&["symbol:Config"])
        );
        assert_eq!(
            candidates(Vec::new(), "scope:tests:t", "Config"),
            ids(&[]),
            "the file's type is not in scope inside a `mod` block without an import"
        );
        assert_eq!(
            candidates(
                vec![glob_import("scope:tests", "super::*")],
                "scope:tests:t",
                "Config"
            ),
            ids(&["symbol:Config"])
        );
        assert_eq!(
            candidates(Vec::new(), "scope:tests:t", "Local"),
            ids(&["symbol:tests:Local"])
        );
        assert_eq!(
            candidates(
                vec![import_binding(
                    "scope:tests",
                    "Config",
                    "super::Config",
                    None
                )],
                "scope:tests:t",
                "Config"
            ),
            ids(&["symbol:Config"]),
            "`use super::Config;` names the file's type"
        );
    }

    #[test]
    fn imported_item_call_is_proven_with_import_binding_and_qualified_name_proofs() {
        // `use crate::auth::issue_token as run;` bound by the import registry, then `run()`.
        with_resolution_context(
            vec![symbol(
                "symbol:issue_token",
                "issue_token",
                "file:src/auth.rs",
                None,
            )],
            Vec::new(),
            vec![import_binding(
                "global",
                "run",
                "crate::auth::issue_token",
                Some("symbol:issue_token"),
            )],
            vec![scope("scope:file", None, ScopeKind::File)],
            |ctx| match resolve_bare_call_outcome(&bare_call(), ctx) {
                ResolutionOutcome::Proven { candidate } => {
                    assert_eq!(candidate.target_symbol_id.0, "symbol:issue_token");
                    let kinds = candidate
                        .proofs
                        .iter()
                        .map(|proof| proof.kind)
                        .collect::<BTreeSet<_>>();
                    assert_eq!(
                        kinds,
                        BTreeSet::from([
                            RelationshipProofKind::ExactCallSite,
                            RelationshipProofKind::ImportBinding,
                            RelationshipProofKind::QualifiedName,
                        ])
                    );
                    assert_eq!(
                        candidate.authority(&GraphEdgeType::Calls),
                        RelationshipAuthority::Authoritative
                    );
                    assert!(
                        candidate
                            .proofs
                            .iter()
                            .filter(|proof| proof.kind != RelationshipProofKind::ExactCallSite)
                            .all(|proof| proof.resolver_strategy == "rust_item_import"),
                        "{:?}",
                        candidate.proofs
                    );
                }
                other => panic!("expected proven imported call, got {other:?}"),
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
