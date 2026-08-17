from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    actual = text.count(old)
    if actual != count:
        raise SystemExit(f"{label}: expected {count} anchor(s), found {actual}")
    return text.replace(old, new, count)


architecture = Path("crates/open-kioku-architecture/src/lib.rs")
text = architecture.read_text()

anchor = """#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub components: Vec<ArchitectureComponent>,
    pub unmapped_targets: Vec<UnmappedPolicyTarget>,
    pub violations: Vec<PolicyViolation>,
}

pub fn evaluate_policy<S>(
"""
replacement = """#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub components: Vec<ArchitectureComponent>,
    pub unmapped_targets: Vec<UnmappedPolicyTarget>,
    pub violations: Vec<PolicyViolation>,
}

/// Controls whether architecture-policy evaluation consumes every persisted structural edge or
/// only relationships that satisfy the typed RI3 authority contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RelationshipEvidenceMode {
    #[default]
    All,
    AuthoritativeOnly,
}

pub fn evaluate_policy<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PolicyCheckReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    evaluate_policy_with_relationship_mode(store, resolver, policy, RelationshipEvidenceMode::All)
}

/// Evaluate architecture policy using only proof-authorized structural relationships.
///
/// This explicit opt-in preserves pre-RI3 behavior for existing callers until the resolver emits
/// typed proof for all supported structural relationships.
pub fn evaluate_policy_authoritative<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PolicyCheckReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    evaluate_policy_with_relationship_mode(
        store,
        resolver,
        policy,
        RelationshipEvidenceMode::AuthoritativeOnly,
    )
}

pub fn evaluate_policy_with_relationship_mode<S>(
"""
text = replace_exact(text, anchor, replacement, "architecture policy wrapper seam")
text = replace_exact(
    text,
    """pub fn evaluate_policy_with_relationship_mode<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PolicyCheckReport>
""",
    """pub fn evaluate_policy_with_relationship_mode<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
    relationship_mode: RelationshipEvidenceMode,
) -> Result<PolicyCheckReport>
""",
    "architecture mode parameter",
)
text = replace_exact(
    text,
    """                if !edge.is_authoritative_relationship() {
                    ignored_non_authoritative_edges += 1;
                    continue;
                }
""",
    """                if relationship_mode == RelationshipEvidenceMode::AuthoritativeOnly
                    && !edge.is_authoritative_relationship()
                {
                    ignored_non_authoritative_edges += 1;
                    continue;
                }
""",
    "architecture policy authority conditional",
    count=2,
)
text = replace_exact(
    text,
    """    if ignored_non_authoritative_edges > 0 {
        report.uncertainty.push(format!(
            "ignored {} non-authoritative structural edge(s); architecture enforcement requires typed relationship proof",
            ignored_non_authoritative_edges
        ));
    }
    if report.evaluated_edge_count == 0 {
        report.uncertainty.push(
            "no authoritative import, reference, or call graph edges were available to evaluate"
                .into(),
        );
    }
""",
    """    if ignored_non_authoritative_edges > 0 {
        report.uncertainty.push(format!(
            "ignored {} non-authoritative structural edge(s); authoritative-only architecture evaluation requires typed relationship proof",
            ignored_non_authoritative_edges
        ));
    }
    if report.evaluated_edge_count == 0 {
        report.uncertainty.push(
            match relationship_mode {
                RelationshipEvidenceMode::All => {
                    "no import, reference, or call graph edges were available to evaluate"
                }
                RelationshipEvidenceMode::AuthoritativeOnly => {
                    "no authoritative import, reference, or call graph edges were available to evaluate"
                }
            }
            .into(),
        );
    }
""",
    "architecture policy uncertainty mode",
)

public_anchor = """pub fn evaluate_public_api_boundary<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PublicApiBoundaryReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
"""
public_replacement = """pub fn evaluate_public_api_boundary<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PublicApiBoundaryReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    evaluate_public_api_boundary_with_relationship_mode(
        store,
        resolver,
        policy,
        RelationshipEvidenceMode::All,
    )
}

/// Evaluate public-API boundaries using only proof-authorized structural relationships.
pub fn evaluate_public_api_boundary_authoritative<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PublicApiBoundaryReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    evaluate_public_api_boundary_with_relationship_mode(
        store,
        resolver,
        policy,
        RelationshipEvidenceMode::AuthoritativeOnly,
    )
}

pub fn evaluate_public_api_boundary_with_relationship_mode<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
    relationship_mode: RelationshipEvidenceMode,
) -> Result<PublicApiBoundaryReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
"""
text = replace_exact(text, public_anchor, public_replacement, "public API mode wrapper seam")
text = replace_exact(
    text,
    """    if ignored_non_authoritative_edges > 0 {
        report.uncertainty.push(format!(
            "ignored {} non-authoritative structural edge(s); architecture enforcement requires typed relationship proof",
            ignored_non_authoritative_edges
        ));
    }
    if report.evaluated_edge_count == 0 {
        report.uncertainty.push(
            "no authoritative import, reference, or call graph edges were available to evaluate"
                .into(),
        );
    }
""",
    """    if ignored_non_authoritative_edges > 0 {
        report.uncertainty.push(format!(
            "ignored {} non-authoritative structural edge(s); authoritative-only public API evaluation requires typed relationship proof",
            ignored_non_authoritative_edges
        ));
    }
    if report.evaluated_edge_count == 0 {
        report.uncertainty.push(
            match relationship_mode {
                RelationshipEvidenceMode::All => {
                    "no import, reference, or call graph edges were available to evaluate"
                }
                RelationshipEvidenceMode::AuthoritativeOnly => {
                    "no authoritative import, reference, or call graph edges were available to evaluate"
                }
            }
            .into(),
        );
    }
""",
    "public API uncertainty mode",
)

# The authority regression should exercise the explicit consumer path, not change default semantics.
helper_anchor = """    fn evaluate(
        files: &[File],
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        policy: &ArchitecturePolicy,
    ) -> PolicyCheckReport {
        let store = store_with_graph(files, nodes, edges);
        let resolver = PolicyResolver::new(policy).expect("resolver");
        evaluate_policy(&store, &resolver, policy).expect("policy evaluation")
    }

"""
helper_replacement = helper_anchor + """    fn evaluate_authoritative(
        files: &[File],
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        policy: &ArchitecturePolicy,
    ) -> PolicyCheckReport {
        let store = store_with_graph(files, nodes, edges);
        let resolver = PolicyResolver::new(policy).expect("resolver");
        evaluate_policy_authoritative(&store, &resolver, policy)
            .expect("authoritative policy evaluation")
    }

"""
text = replace_exact(text, helper_anchor, helper_replacement, "architecture test helper")
text = replace_exact(
    text,
    """        let report = evaluate(
            &[domain.clone(), api.clone()],
            &[domain_node.clone(), api_node.clone()],
            &[unproved_edge(
                "high-confidence-without-proof",
                &domain_node,
                &api_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );
""",
    """        let report = evaluate_authoritative(
            &[domain.clone(), api.clone()],
            &[domain_node.clone(), api_node.clone()],
            &[unproved_edge(
                "high-confidence-without-proof",
                &domain_node,
                &api_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );
""",
    "architecture authoritative smoke test",
)
# Add a compatibility regression ensuring the default path still evaluates legacy edges.
compat_test_anchor = """    #[test]
    fn forbidden_dependency_rule_reports_deterministic_violation() {"""
compat_test = """    #[test]
    fn default_policy_evaluation_preserves_legacy_structural_edges() {
        let domain = file("domain", "src/domain/order.rs");
        let api = file("api", "src/api/http.rs");
        let domain_node = file_node(&domain);
        let api_node = file_node(&api);
        let policy = policy(vec![DependencyRule {
            id: "domain-must-not-call-api".into(),
            from: "domain".into(),
            to: "api".into(),
            action: DependencyAction::Forbid,
            severity: Severity::Error,
            reason: "domain cannot depend on api".into(),
        }]);

        let report = evaluate(
            &[domain.clone(), api.clone()],
            &[domain_node.clone(), api_node.clone()],
            &[unproved_edge(
                "legacy-high-confidence",
                &domain_node,
                &api_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );

        assert_eq!(report.evaluated_edge_count, 1);
        assert_eq!(report.violation_count, 1);
    }

    #[test]
    fn forbidden_dependency_rule_reports_deterministic_violation() {"""
text = replace_exact(text, compat_test_anchor, compat_test, "architecture compatibility regression")
architecture.write_text(text)


context = Path("crates/open-kioku-context/src/lib.rs")
text = context.read_text()
old_helper = """fn extend_authoritative_relationships(target: &mut Vec<GraphEdge>, edges: Vec<GraphEdge>) {
    target.extend(
        edges
            .into_iter()
            .filter(GraphEdge::is_authoritative_relationship),
    );
}
"""
new_helper = """/// Return only dependency edges that satisfy the typed structural relationship authority contract.
///
/// ContextPack generation keeps legacy graph behavior until proof-bearing resolver emission is the
/// default. Consumers that require structural truth can explicitly request this fail-closed view.
pub fn authoritative_dependency_edges(
    edges: impl IntoIterator<Item = GraphEdge>,
) -> Vec<GraphEdge> {
    edges
        .into_iter()
        .filter(GraphEdge::is_authoritative_relationship)
        .collect()
}
"""
text = replace_exact(text, old_helper, new_helper, "context authoritative helper")
text = replace_exact(
    text,
    "                extend_authoritative_relationships(&mut dependency_edges, edges);\n",
    "                dependency_edges.extend(edges);\n",
    "context default dependency behavior",
)
text = replace_exact(
    text,
    """        let mut selected = Vec::new();
        extend_authoritative_relationships(&mut selected, vec![legacy, proved]);

        assert_eq!(selected.len(), 1);
""",
    """        let selected = authoritative_dependency_edges(vec![legacy, proved]);

        assert_eq!(selected.len(), 1);
""",
    "context explicit authoritative smoke test",
)
context.write_text(text)
