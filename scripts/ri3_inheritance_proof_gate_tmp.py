from pathlib import Path

core = Path("crates/open-kioku-core/src/relationship.rs")
text = core.read_text()
old_policy = '''        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope)))
        }
'''
new_policy = '''        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || same_scope
                    || (receiver_type && (qualified_name || containing_type))
                    || (import_binding && (qualified_name || same_scope))
                    || (inheritance_binding && (receiver_type || containing_type)))
        }
'''
if text.count(old_policy) != 1:
    raise SystemExit(f"core Calls policy seam changed: {text.count(old_policy)}")
text = text.replace(old_policy, new_policy, 1)

anchor = '''    #[test]
    fn conflicting_target_ids_fail_closed() {
'''
tests = '''    #[test]
    fn unique_lexical_scope_definition_authorizes_call() {
        let proved = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn ambiguous_lexical_scope_definition_does_not_authorize_call() {
        let ambiguous = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 2),
            ],
        );
        assert_ne!(
            ambiguous.relationship_authority(),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn unique_inheritance_binding_authorizes_call() {
        let proved = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::InheritanceBinding, 1),
                proof(RelationshipProofKind::ContainingType, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn ambiguous_inheritance_binding_does_not_authorize_call() {
        let ambiguous = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::InheritanceBinding, 2),
                proof(RelationshipProofKind::ContainingType, 2),
            ],
        );
        assert_ne!(
            ambiguous.relationship_authority(),
            RelationshipAuthority::Authoritative
        );
    }

'''
if text.count(anchor) != 1:
    raise SystemExit(f"core Calls test anchor changed: {text.count(anchor)}")
text = text.replace(anchor, tests + anchor, 1)
core.write_text(text)

calls = Path("crates/open-kioku-resolution/src/call_candidates.rs")
text = calls.read_text()
old_super = '''fn discover_super_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(caller_id) = call.caller_symbol_id.as_ref() else {
        return Vec::new();
    };
    let Some(caller) = ctx.symbols.get(caller_id) else {
        return Vec::new();
    };
    let Some(containing_type) = caller.parent_symbol_id.as_ref() else {
        return Vec::new();
    };
    let Some(target) =
        ctx.inheritance
            .resolve_inherited_member(containing_type, &call.callee_name, ctx.symbols)
    else {
        return Vec::new();
    };

    // Keep inherited-member discovery heuristic until the inheritance index itself returns all
    // viable parents/members rather than a first traversal hit. This prevents BFS order becoming
    // structural truth during the migration.
    let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::High)
        .with_strategy(ResolutionStrategy::Inheritance);
    candidate.evidence.push(resolution_evidence(
        call,
        ctx,
        target,
        ResolutionEvidenceKind::InheritanceGraph,
        "inherited member candidate retained pending proof-complete inheritance discovery",
    ));
    vec![candidate]
}
'''
new_super = '''fn discover_super_candidates(
    call: &CallSite,
    ctx: &ResolutionContext<'_>,
) -> Vec<ResolutionCandidate> {
    let Some(caller_id) = call.caller_symbol_id.as_ref() else {
        return Vec::new();
    };
    let Some(caller) = ctx.symbols.get(caller_id) else {
        return Vec::new();
    };
    let Some(containing_type) = caller.parent_symbol_id.as_ref() else {
        return Vec::new();
    };
    let targets = ctx.inheritance.inherited_member_candidates(
        containing_type,
        &call.callee_name,
        ctx.symbols,
    );
    candidates_for_targets(
        call,
        ctx,
        targets,
        CandidateTemplate {
            confidence: Confidence::Exact,
            strategy: ResolutionStrategy::Inheritance,
            proof_kinds: &[
                RelationshipProofKind::InheritanceBinding,
                RelationshipProofKind::ContainingType,
            ],
            evidence_kind: ResolutionEvidenceKind::InheritanceGraph,
            message: "super-call candidate discovered from nearest inheritance binding",
        },
    )
}
'''
if text.count(old_super) != 1:
    raise SystemExit(f"super candidate seam changed: {text.count(old_super)}")
text = text.replace(old_super, new_super, 1)
calls.write_text(text)
