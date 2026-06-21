pub mod resolver;

pub use resolver::PolicyResolver;

use open_kioku_config::{ArchitecturePolicy, DependencyAction, DependencyRule};
use open_kioku_core::{
    ArchitectureComponent, EnforcedEdgeType, GraphEdge, GraphNode, PolicyCheckReport,
    PolicyMatchEvidence, PolicyViolation, UnknownPolicyEdge, UnmappedPolicyTarget,
};
use open_kioku_errors::Result;
use open_kioku_storage::{GraphStore, MetadataStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_UNKNOWN_EDGE_SAMPLES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub components: Vec<ArchitectureComponent>,
    pub unmapped_targets: Vec<UnmappedPolicyTarget>,
    pub violations: Vec<PolicyViolation>,
}

pub fn evaluate_policy<S>(
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
) -> Result<PolicyCheckReport>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    let files = store.list_files(usize::MAX, 0)?;
    let file_paths = files
        .iter()
        .map(|file| (file.id.0.clone(), file.path.clone()))
        .collect::<BTreeMap<_, _>>();
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let symbol_paths = symbols
        .iter()
        .filter_map(|symbol| {
            file_paths
                .get(&symbol.file_id.0)
                .map(|path| (symbol.id.0.clone(), path.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    let mut report = PolicyCheckReport {
        configured: true,
        ..PolicyCheckReport::default()
    };

    for edge_type in [
        EnforcedEdgeType::Imports,
        EnforcedEdgeType::References,
        EnforcedEdgeType::Calls,
    ] {
        let graph_edge_type = edge_type.graph_edge_type();
        let mut offset = 0;
        loop {
            let batch = store.edges_by_type(graph_edge_type.clone(), 1_000, offset)?;
            if batch.is_empty() {
                break;
            }
            for edge in &batch {
                report.evaluated_edge_count += 1;
                evaluate_edge(
                    &mut report,
                    store,
                    resolver,
                    policy,
                    edge,
                    edge_type,
                    &file_paths,
                    &symbol_paths,
                )?;
            }
            offset += batch.len();
            if batch.len() < 1_000 {
                break;
            }
        }
    }

    report.violation_count = report.violations.len();
    report.unknown_sample_count = report.unknown_edges.len();
    report.unknown_edges_truncated = report.unknown_edge_count > report.unknown_edges.len();
    if report.evaluated_edge_count == 0 {
        report
            .uncertainty
            .push("no import, reference, or call graph edges were available to evaluate".into());
    }
    if report.unknown_edge_count > 0 {
        report.uncertainty.push(format!(
            "{} dependency edge(s) could not be mapped to explicit policy rules or components",
            report.unknown_edge_count
        ));
    }
    report.violations.sort_by(|left, right| {
        left.rule_id
            .cmp(&right.rule_id)
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.edge_type.cmp(&right.edge_type))
    });
    report.violations.dedup();
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_edge(
    report: &mut PolicyCheckReport,
    store: &(impl GraphStore + ?Sized),
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
    edge: &GraphEdge,
    edge_type: EnforcedEdgeType,
    file_paths: &BTreeMap<String, PathBuf>,
    symbol_paths: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    let Some(evidence) = edge_evidence(store, edge, edge_type, file_paths, symbol_paths)? else {
        return Ok(());
    };
    if evidence.source_path == evidence.target_path {
        report.allowed_edges += 1;
        return Ok(());
    }
    let source = resolver.resolve_node(&evidence.source_path, edge.evidence.symbol_id.clone());
    let target = resolver.resolve_node(&evidence.target_path, None);
    let (source, target) = match (source, target) {
        (Ok(source), Ok(target)) => (source, target),
        (Err(_), _) | (_, Err(_)) => {
            add_unknown(
                report,
                "source or target path did not match any architecture policy component",
                evidence,
            );
            return Ok(());
        }
    };

    let mut matched_rule = false;
    let mut edge_forbidden = false;
    for from_component in &source.components {
        for to_component in &target.components {
            if from_component.component_id == to_component.component_id {
                report.allowed_edges += 1;
                matched_rule = true;
                continue;
            }
            let matching_rules = policy
                .dependency_rules
                .iter()
                .filter(|rule| {
                    rule_matches(
                        rule,
                        &from_component.component_id,
                        &to_component.component_id,
                    )
                })
                .collect::<Vec<_>>();
            if matching_rules.is_empty() {
                continue;
            }
            matched_rule = true;
            for rule in matching_rules {
                match rule.action {
                    DependencyAction::Allow => {
                        report.allowed_edges += 1;
                    }
                    DependencyAction::Forbid => {
                        edge_forbidden = true;
                        report.violations.push(PolicyViolation {
                            rule_id: rule.id.clone(),
                            severity: format!("{:?}", rule.severity).to_ascii_lowercase(),
                            source_component: from_component.component_id.clone(),
                            target_component: to_component.component_id.clone(),
                            source_path: evidence.source_path.clone(),
                            target_path: evidence.target_path.clone(),
                            edge_type,
                            evidence: evidence.clone(),
                            message: rule.reason.clone(),
                        });
                    }
                }
            }
        }
    }
    if !matched_rule {
        add_unknown(
            report,
            "no dependency rule matched this source and target component pair",
            evidence,
        );
    } else if edge_forbidden {
        // Forbidden edges are counted by violation_count after deterministic de-duplication.
    }
    Ok(())
}

fn add_unknown(report: &mut PolicyCheckReport, reason: &str, evidence: PolicyMatchEvidence) {
    report.unknown_edge_count += 1;
    if report.unknown_edges.len() < MAX_UNKNOWN_EDGE_SAMPLES {
        report.unknown_edges.push(UnknownPolicyEdge {
            reason: reason.into(),
            evidence,
        });
    }
}

fn rule_matches(rule: &DependencyRule, from_component: &str, to_component: &str) -> bool {
    (rule.from == "*" || rule.from == from_component) && (rule.to == "*" || rule.to == to_component)
}

fn edge_evidence(
    store: &(impl GraphStore + ?Sized),
    edge: &GraphEdge,
    edge_type: EnforcedEdgeType,
    file_paths: &BTreeMap<String, PathBuf>,
    symbol_paths: &BTreeMap<String, PathBuf>,
) -> Result<Option<PolicyMatchEvidence>> {
    let Some(source_node) = store.node_by_id(&edge.from.0)? else {
        return Ok(None);
    };
    let Some(target_node) = store.node_by_id(&edge.to.0)? else {
        return Ok(None);
    };
    let Some(source_path) = node_path(&source_node, file_paths, symbol_paths) else {
        return Ok(None);
    };
    let Some(target_path) = node_path(&target_node, file_paths, symbol_paths) else {
        return Ok(None);
    };
    Ok(Some(PolicyMatchEvidence {
        edge_id: edge.id.0.clone(),
        edge_type,
        source_node: edge.from.0.clone(),
        target_node: edge.to.0.clone(),
        source_path,
        target_path,
        confidence: edge.evidence.confidence,
        message: edge.evidence.message.clone(),
    }))
}

fn node_path(
    node: &GraphNode,
    file_paths: &BTreeMap<String, PathBuf>,
    symbol_paths: &BTreeMap<String, PathBuf>,
) -> Option<PathBuf> {
    node.file_id
        .as_ref()
        .and_then(|id| file_paths.get(&id.0))
        .cloned()
        .or_else(|| {
            node.symbol_id
                .as_ref()
                .and_then(|id| symbol_paths.get(&id.0))
                .cloned()
        })
        .or_else(|| {
            if node.node_type == open_kioku_core::GraphNodeType::File {
                Some(Path::new(&node.label).to_path_buf())
            } else {
                None
            }
        })
}

pub struct ArchitectureDetector<'a> {
    store: &'a dyn MetadataStore,
    resolver: Option<&'a PolicyResolver>,
}

impl<'a> ArchitectureDetector<'a> {
    pub fn new(store: &'a dyn MetadataStore, resolver: Option<&'a PolicyResolver>) -> Self {
        Self { store, resolver }
    }

    pub fn detect(&self) -> Result<ArchitectureSummary> {
        let files = self.store.list_files(usize::MAX, 0)?;

        let mut unmapped_targets = Vec::new();

        if let Some(resolver) = self.resolver {
            let mut component_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

            for file in files {
                let path = file.path.display().to_string();
                let resolution = resolver.resolve_node(&path, None);
                match resolution {
                    Ok(node) => {
                        for comp in node.components {
                            component_map
                                .entry(comp.component_id)
                                .or_default()
                                .push(path.clone());
                        }
                    }
                    Err(unmapped) => {
                        unmapped_targets.push(unmapped);
                    }
                }
            }

            let components = component_map
                .into_iter()
                .map(|(id, mut paths)| {
                    paths.sort();
                    paths.dedup();
                    ArchitectureComponent {
                        id: id.clone(),
                        name: id,
                        paths,
                        evidence: Vec::new(),
                    }
                })
                .collect();

            Ok(ArchitectureSummary {
                components,
                unmapped_targets,
                violations: Vec::new(),
            })
        } else {
            // Fallback for missing policy (or for test compatibility)
            let mut components = Vec::new();
            for (name, marker) in [
                ("api", "/api/"),
                ("service", "/service"),
                ("repository", "/repo"),
                ("tests", "/test"),
                ("configuration", "config"),
            ] {
                let paths = files
                    .iter()
                    .filter(|file| {
                        file.path
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains(marker)
                    })
                    .map(|file| file.path.display().to_string())
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    components.push(ArchitectureComponent {
                        id: name.into(),
                        name: name.into(),
                        paths,
                        evidence: Vec::new(),
                    });
                }
            }
            Ok(ArchitectureSummary {
                components,
                unmapped_targets,
                violations: Vec::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use open_kioku_config::{
        DependencyAction, DependencyRule, PolicyLayer, PolicyVersion, Severity,
    };
    use open_kioku_core::{
        Confidence, EdgeId, Evidence, File, FileId, GraphEdgeType, GraphNodeType, IndexManifest,
        IndexMode, IndexQuality, Language, NodeId, Repository, RepositoryId,
    };
    use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
    use open_kioku_storage_sqlite::SqliteStore;
    use std::path::PathBuf;

    fn store_with_graph(files: &[File], nodes: &[GraphNode], edges: &[GraphEdge]) -> SqliteStore {
        let store = SqliteStore::open(":memory:").expect("in-memory sqlite store");
        store.initialize().expect("initialize store");
        let manifest = IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: Some(Utc::now()),
            },
            file_count: files.len(),
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: IndexMode::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files,
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
            })
            .expect("replace index");
        store.replace_graph(nodes, edges).expect("replace graph");
        store
    }

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 10,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn file_node(file: &File) -> GraphNode {
        GraphNode {
            id: NodeId::new(format!("file:{}", file.path.display())),
            node_type: GraphNodeType::File,
            label: file.path.display().to_string(),
            file_id: Some(file.id.clone()),
            ..GraphNode::default()
        }
    }

    fn edge(id: &str, from: &GraphNode, to: &GraphNode, edge_type: GraphEdgeType) -> GraphEdge {
        GraphEdge {
            id: EdgeId::new(id),
            from: from.id.clone(),
            to: to.id.clone(),
            edge_type,
            evidence: Evidence {
                id: open_kioku_core::EvidenceId::new(format!("evidence-{id}")),
                source: "test".into(),
                confidence: Confidence::High,
                message: format!("{id} evidence"),
                ..Evidence::default()
            },
            ..GraphEdge::default()
        }
    }

    fn policy(rules: Vec<DependencyRule>) -> ArchitecturePolicy {
        ArchitecturePolicy {
            version: PolicyVersion::V1,
            layers: vec![
                PolicyLayer {
                    id: "domain".into(),
                    description: None,
                    paths: vec!["src/domain/**".into()],
                },
                PolicyLayer {
                    id: "api".into(),
                    description: None,
                    paths: vec!["src/api/**".into()],
                },
            ],
            contexts: Vec::new(),
            dependency_rules: rules,
            public_api_rules: Vec::new(),
            exemptions: Vec::new(),
            source: Default::default(),
        }
    }

    fn evaluate(
        files: &[File],
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        policy: &ArchitecturePolicy,
    ) -> PolicyCheckReport {
        let store = store_with_graph(files, nodes, edges);
        let resolver = PolicyResolver::new(policy).expect("resolver");
        evaluate_policy(&store, &resolver, policy).expect("policy evaluation")
    }

    #[test]
    fn forbidden_dependency_rule_reports_deterministic_violation() {
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
            &[edge(
                "call-domain-api",
                &domain_node,
                &api_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );

        assert!(report.configured);
        assert_eq!(report.evaluated_edge_count, 1);
        assert_eq!(report.violation_count, 1);
        assert_eq!(report.unknown_edge_count, 0);
        let violation = &report.violations[0];
        assert_eq!(violation.rule_id, "domain-must-not-call-api");
        assert_eq!(violation.severity, "error");
        assert_eq!(violation.source_component, "domain");
        assert_eq!(violation.target_component, "api");
        assert_eq!(violation.source_path, PathBuf::from("src/domain/order.rs"));
        assert_eq!(violation.target_path, PathBuf::from("src/api/http.rs"));
        assert_eq!(violation.edge_type, EnforcedEdgeType::Calls);
        assert_eq!(violation.evidence.edge_id, "call-domain-api");
        assert_eq!(violation.evidence.confidence, Confidence::High);
    }

    #[test]
    fn allow_rule_counts_allowed_edge_without_violation() {
        let api = file("api", "src/api/http.rs");
        let domain = file("domain", "src/domain/order.rs");
        let api_node = file_node(&api);
        let domain_node = file_node(&domain);
        let policy = policy(vec![DependencyRule {
            id: "api-can-import-domain".into(),
            from: "api".into(),
            to: "domain".into(),
            action: DependencyAction::Allow,
            severity: Severity::Info,
            reason: "api composes domain services".into(),
        }]);

        let report = evaluate(
            &[api.clone(), domain.clone()],
            &[api_node.clone(), domain_node.clone()],
            &[edge(
                "import-api-domain",
                &api_node,
                &domain_node,
                GraphEdgeType::Imports,
            )],
            &policy,
        );

        assert_eq!(report.evaluated_edge_count, 1);
        assert_eq!(report.allowed_edges, 1);
        assert_eq!(report.violation_count, 0);
        assert_eq!(report.unknown_edge_count, 0);
    }

    #[test]
    fn unknown_edges_are_counted_exactly_with_bounded_sample() {
        let domain = file("domain", "src/domain/order.rs");
        let api = file("api", "src/api/http.rs");
        let domain_node = file_node(&domain);
        let api_node = file_node(&api);
        let edges = (0..=MAX_UNKNOWN_EDGE_SAMPLES)
            .map(|index| {
                edge(
                    &format!("reference-{index:03}"),
                    &domain_node,
                    &api_node,
                    GraphEdgeType::References,
                )
            })
            .collect::<Vec<_>>();
        let policy = policy(Vec::new());

        let report = evaluate(
            &[domain.clone(), api.clone()],
            &[domain_node, api_node],
            &edges,
            &policy,
        );

        assert_eq!(report.evaluated_edge_count, MAX_UNKNOWN_EDGE_SAMPLES + 1);
        assert_eq!(report.unknown_edge_count, MAX_UNKNOWN_EDGE_SAMPLES + 1);
        assert_eq!(report.unknown_sample_count, MAX_UNKNOWN_EDGE_SAMPLES);
        assert_eq!(report.unknown_edges.len(), MAX_UNKNOWN_EDGE_SAMPLES);
        assert!(report.unknown_edges_truncated);
        assert_eq!(
            report.unknown_edges[0].reason,
            "no dependency rule matched this source and target component pair"
        );
        assert_eq!(report.unknown_edges[0].evidence.edge_id, "reference-000");
    }

    #[test]
    fn unmapped_policy_component_is_reported_as_unknown() {
        let domain = file("domain", "src/domain/order.rs");
        let vendor = file("vendor", "vendor/client.rs");
        let domain_node = file_node(&domain);
        let vendor_node = file_node(&vendor);
        let policy = policy(vec![DependencyRule {
            id: "domain-can-call-api".into(),
            from: "domain".into(),
            to: "api".into(),
            action: DependencyAction::Allow,
            severity: Severity::Info,
            reason: "not applicable to vendor".into(),
        }]);

        let report = evaluate(
            &[domain.clone(), vendor.clone()],
            &[domain_node.clone(), vendor_node.clone()],
            &[edge(
                "call-domain-vendor",
                &domain_node,
                &vendor_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );

        assert_eq!(report.evaluated_edge_count, 1);
        assert_eq!(report.violation_count, 0);
        assert_eq!(report.unknown_edge_count, 1);
        assert_eq!(
            report.unknown_edges[0].reason,
            "source or target path did not match any architecture policy component"
        );
        assert_eq!(
            report.unknown_edges[0].evidence.target_path,
            PathBuf::from("vendor/client.rs")
        );
    }
}
