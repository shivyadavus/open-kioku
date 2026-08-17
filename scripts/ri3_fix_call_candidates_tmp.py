from pathlib import Path

path = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = path.read_text()

self_call = """candidates_for_targets(
        call,
        targets,
        Confidence::Exact,
        ResolutionStrategy::ImplicitSelf,
        &[
            RelationshipProofKind::ReceiverType,
            RelationshipProofKind::ContainingType,
            RelationshipProofKind::SameScopeDefinition,
        ],
        ResolutionEvidenceKind::ImplicitSelf,
        \"resolved explicit self/this member from containing type\",
    )
"""
self_call_new = """candidates_for_targets(
        call,
        ctx,
        targets,
        CandidateTemplate {
            confidence: Confidence::Exact,
            strategy: ResolutionStrategy::ImplicitSelf,
            proof_kinds: &[
                RelationshipProofKind::ReceiverType,
                RelationshipProofKind::ContainingType,
                RelationshipProofKind::SameScopeDefinition,
            ],
            evidence_kind: ResolutionEvidenceKind::ImplicitSelf,
            message: \"resolved explicit self/this member from containing type\",
        },
    )
"""
if text.count(self_call) != 1:
    raise SystemExit(f"unexpected self candidate call seam: {text.count(self_call)}")
text = text.replace(self_call, self_call_new, 1)

bare_call = """candidates_for_targets(
                call,
                targets,
                Confidence::Exact,
                ResolutionStrategy::LexicalScope,
                &[RelationshipProofKind::SameScopeDefinition],
                ResolutionEvidenceKind::LexicalScope,
                \"bare-call candidate discovered in exact lexical scope\",
            );
"""
bare_call_new = """candidates_for_targets(
                call,
                ctx,
                targets,
                CandidateTemplate {
                    confidence: Confidence::Exact,
                    strategy: ResolutionStrategy::LexicalScope,
                    proof_kinds: &[RelationshipProofKind::SameScopeDefinition],
                    evidence_kind: ResolutionEvidenceKind::LexicalScope,
                    message: \"bare-call candidate discovered in exact lexical scope\",
                },
            );
"""
if text.count(bare_call) != 1:
    raise SystemExit(f"unexpected bare candidate call seam: {text.count(bare_call)}")
text = text.replace(bare_call, bare_call_new, 1)

old_helper = """fn candidates_for_targets(
    call: &CallSite,
    targets: Vec<SymbolId>,
    confidence: Confidence,
    strategy: ResolutionStrategy,
    proof_kinds: &[RelationshipProofKind],
    evidence_kind: ResolutionEvidenceKind,
    message: &str,
) -> Vec<ResolutionCandidate> {
    let count = targets.len();
    targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), confidence)
                .with_strategy(strategy);
            candidate.proofs.push(call_site_proof(call, &dummy_context_path(ctx_path_placeholder()), &target));
            candidate
        })
        .collect()
}
"""
new_helper = """struct CandidateTemplate<'a> {
    confidence: Confidence,
    strategy: ResolutionStrategy,
    proof_kinds: &'a [RelationshipProofKind],
    evidence_kind: ResolutionEvidenceKind,
    message: &'a str,
}

fn candidates_for_targets(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
    targets: Vec<SymbolId>,
    template: CandidateTemplate<'_>,
) -> Vec<ResolutionCandidate> {
    let count = targets.len();
    targets
        .into_iter()
        .map(|target| {
            let mut candidate = ResolutionCandidate::new(target.clone(), template.confidence)
                .with_strategy(template.strategy);
            candidate.proofs.push(call_site_proof(call, ctx, &target));
            for kind in template.proof_kinds {
                candidate.proofs.push(proof(
                    call,
                    ctx,
                    &target,
                    *kind,
                    strategy_name(template.strategy),
                    count,
                ));
            }
            candidate.evidence.push(resolution_evidence(
                call,
                ctx,
                target,
                template.evidence_kind.clone(),
                template.message,
            ));
            candidate
        })
        .collect()
}

fn strategy_name(strategy: ResolutionStrategy) -> &'static str {
    match strategy {
        ResolutionStrategy::ExactOccurrence => \"exact_occurrence\",
        ResolutionStrategy::LexicalScope => \"lexical_scope\",
        ResolutionStrategy::ImplicitSelf => \"implicit_self\",
        ResolutionStrategy::TypedReceiver => \"typed_receiver\",
        ResolutionStrategy::StaticReceiver => \"static_receiver\",
        ResolutionStrategy::ExactImportBinding => \"exact_import_binding\",
        ResolutionStrategy::ModuleExport => \"module_export\",
        ResolutionStrategy::SameFile => \"same_file\",
        ResolutionStrategy::Inheritance => \"inheritance\",
        ResolutionStrategy::QualifiedName => \"qualified_name\",
        ResolutionStrategy::ExternalExactIndex => \"external_exact_index\",
        ResolutionStrategy::Heuristic => \"heuristic\",
    }
}
"""
if text.count(old_helper) != 1:
    raise SystemExit(f"candidate helper seam changed: {text.count(old_helper)}")
text = text.replace(old_helper, new_helper, 1)

old_details = """    proof.details.insert(
        \"start_column\".into(),
        serde_json::Value::from(call.range.start_column),
    );
    proof.details.insert(
        \"end_column\".into(),
        serde_json::Value::from(call.range.end_column),
    );
"""
if text.count(old_details) != 1:
    raise SystemExit(f"column details seam changed: {text.count(old_details)}")
text = text.replace(old_details, "", 1)

placeholder = """
// Placeholder helpers below are intentionally absent; `candidates_for_targets` is completed by the
// follow-up patch in the same guarded validation slice.
fn ctx_path_placeholder() -> &'static std::path::Path {
    std::path::Path::new(\"\")
}

fn dummy_context_path(_path: &std::path::Path) -> ResolutionContext<'static> {
    unreachable!(\"validation patch must replace candidates_for_targets before compilation\")
}
"""
if text.count(placeholder) != 1:
    raise SystemExit(f"placeholder seam changed: {text.count(placeholder)}")
text = text.replace(placeholder, "\n", 1)

path.write_text(text)
