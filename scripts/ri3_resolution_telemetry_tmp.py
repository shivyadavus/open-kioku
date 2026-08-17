from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

# Stable labels for telemetry; labels are explicit so metrics do not depend on Debug formatting.
pipeline = Path("crates/open-kioku-resolution/src/pipeline.rs")
text = pipeline.read_text()
anchor = '''pub enum ResolutionStrategy {
    ExactOccurrence,
    LexicalScope,
    ImplicitSelf,
    TypedReceiver,
    StaticReceiver,
    ExactImportBinding,
    ModuleExport,
    SameFile,
    Inheritance,
    QualifiedName,
    ExternalExactIndex,
    Heuristic,
}
'''
replacement = anchor + '''
impl ResolutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactOccurrence => "exact_occurrence",
            Self::LexicalScope => "lexical_scope",
            Self::ImplicitSelf => "implicit_self",
            Self::TypedReceiver => "typed_receiver",
            Self::StaticReceiver => "static_receiver",
            Self::ExactImportBinding => "exact_import_binding",
            Self::ModuleExport => "module_export",
            Self::SameFile => "same_file",
            Self::Inheritance => "inheritance",
            Self::QualifiedName => "qualified_name",
            Self::ExternalExactIndex => "external_exact_index",
            Self::Heuristic => "heuristic",
        }
    }
}
'''
text = replace_exact(text, anchor, replacement, "ResolutionStrategy labels")
pipeline.write_text(text)

types = Path("crates/open-kioku-resolution/src/type_candidates.rs")
text = types.read_text()
anchor = '''pub enum TypeDiscovery {
    SameFile,
    ImportBinding,
    QualifiedName,
}
'''
replacement = anchor + '''
impl TypeDiscovery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameFile => "same_file",
            Self::ImportBinding => "import_binding",
            Self::QualifiedName => "qualified_name",
        }
    }
}
'''
text = replace_exact(text, anchor, replacement, "TypeDiscovery labels")
types.write_text(text)

core = Path("crates/open-kioku-core/src/relationship.rs")
text = core.read_text()
anchor = '''impl RelationshipProofKind {
    /// Maximum authority this proof kind can contribute before relationship-specific policy runs.
'''
replacement = '''impl RelationshipProofKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactOccurrence => "exact_occurrence",
            Self::ExactReference => "exact_reference",
            Self::ExactCallSite => "exact_call_site",
            Self::ImportBinding => "import_binding",
            Self::QualifiedName => "qualified_name",
            Self::SameScopeDefinition => "same_scope_definition",
            Self::ContainingType => "containing_type",
            Self::ReceiverType => "receiver_type",
            Self::TraitOrInterfaceBinding => "trait_or_interface_binding",
            Self::InheritanceBinding => "inheritance_binding",
            Self::ModuleOrPackageBinding => "module_or_package_binding",
            Self::ExternalExactIndex => "external_exact_index",
        }
    }

    /// Maximum authority this proof kind can contribute before relationship-specific policy runs.
'''
text = replace_exact(text, anchor, replacement, "RelationshipProofKind labels")
core.write_text(text)

# Inheritance telemetry needs every declaration, including ambiguous/unresolved ones. Also filter
# parent candidates by relationship semantics before binding so IMPLEMENTS never binds a class just
# because its name happened to match.
inheritance = Path("crates/open-kioku-resolution/src/inheritance.rs")
text = inheritance.read_text()
old = '''                let discovered = discover_type_candidates(
                    &child_sym.file_id,
                    child_sym.scope_id.as_ref(),
                    &edge.parent_name,
                    repository,
                    symbols,
                );
'''
new = '''                let discovered = discover_type_candidates(
                    &child_sym.file_id,
                    child_sym.scope_id.as_ref(),
                    &edge.parent_name,
                    repository,
                    symbols,
                )
                .into_iter()
                .filter(|candidate| {
                    symbols
                        .get(&candidate.target)
                        .map(|symbol| inheritance_parent_kind_allowed(&edge.kind, &symbol.kind))
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>();
'''
text = replace_exact(text, old, new, "inheritance semantic parent filter")
old = '''    /// Deterministic view of uniquely bound inheritance declarations for structural emission.
    pub fn resolved_edges(&self) -> Vec<&InheritanceEdge> {
        let mut edges = self
            .edges_by_child
            .values()
            .flatten()
            .filter(|edge| {
                edge.parent_id.is_some()
                    && edge.binding_strategy.is_some()
                    && edge.binding_candidate_count == 1
            })
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            inheritance_edge_order(left, right).then_with(|| {
                left.parent_id
                    .as_ref()
                    .map(|id| id.0.as_str())
                    .cmp(&right.parent_id.as_ref().map(|id| id.0.as_str()))
            })
        });
        edges
    }
'''
new = '''    /// Deterministic view of all inheritance declarations, including ambiguous/unresolved ones.
    pub fn all_edges(&self) -> Vec<&InheritanceEdge> {
        let mut edges = self.edges_by_child.values().flatten().collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            inheritance_edge_order(left, right).then_with(|| {
                left.parent_id
                    .as_ref()
                    .map(|id| id.0.as_str())
                    .cmp(&right.parent_id.as_ref().map(|id| id.0.as_str()))
            })
        });
        edges
    }

    /// Deterministic view of uniquely bound inheritance declarations for structural emission.
    pub fn resolved_edges(&self) -> Vec<&InheritanceEdge> {
        self.all_edges()
            .into_iter()
            .filter(|edge| {
                edge.parent_id.is_some()
                    && edge.binding_strategy.is_some()
                    && edge.binding_candidate_count == 1
            })
            .collect()
    }
'''
text = replace_exact(text, old, new, "inheritance all-edge telemetry view")
anchor = '''fn inheritance_kind_order(kind: &InheritanceKind) -> u8 {
'''
helper = '''fn inheritance_parent_kind_allowed(kind: &InheritanceKind, parent: &SymbolKind) -> bool {
    match kind {
        InheritanceKind::Extends => matches!(
            parent,
            SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
        ),
        InheritanceKind::Implements | InheritanceKind::TraitImpl => {
            matches!(parent, SymbolKind::Trait | SymbolKind::Interface)
        }
        InheritanceKind::Embeds => matches!(
            parent,
            SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
        ),
    }
}

'''
text = replace_exact(text, anchor, helper + anchor, "inheritance parent kind helper")
anchor = '''    #[test]
    fn nearest_inherited_candidates_are_sorted_and_not_first_hit() {
'''
test = '''    #[test]
    fn implements_does_not_bind_class_target() {
        let file = "file:types";
        let child = symbol_in_file("Child", "Child", SymbolKind::Class, None, file);
        let parent = symbol_in_file("Contract", "Contract", SymbolKind::Class, None, file);
        let symbols = SymbolIndex::build(vec![child, parent]);
        let repository = SemanticRepository::new();
        let mut index = InheritanceIndex::build(vec![InheritanceSite {
            child_symbol_id: SymbolId::new("Child"),
            parent_name: "Contract".into(),
            kind: InheritanceKind::Implements,
            order: 0,
            range: range(12),
        }]);

        index.bind_parents_with_repository(&symbols, &repository);
        let edge = index.all_edges()[0];
        assert_eq!(edge.parent_id, None);
        assert_eq!(edge.binding_candidate_count, 0);
    }

'''
text = replace_exact(text, anchor, test + anchor, "inheritance semantic-kind test")
inheritance.write_text(text)

# Extend the one existing quality report rather than inventing a second metrics surface.
ingest = Path("crates/open-kioku-ingest/src/lib.rs")
text = ingest.read_text()
old = '''#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolutionQualityReport {
    pub call_sites: usize,
    pub resolved_exact: usize,
    pub resolved_high: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub legacy_only: usize,
    pub semantic_only: usize,
    pub disagreement: usize,
}
'''
new = '''#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipResolutionTelemetry {
    pub relationship: GraphEdgeType,
    pub language: open_kioku_core::Language,
    pub cases: usize,
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    #[serde(default)]
    pub candidate_count_histogram: BTreeMap<String, usize>,
    #[serde(default)]
    pub strategy_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub proof_counts: BTreeMap<String, usize>,
}

impl RelationshipResolutionTelemetry {
    fn new(relationship: GraphEdgeType, language: open_kioku_core::Language) -> Self {
        Self {
            relationship,
            language,
            cases: 0,
            candidates_considered: 0,
            proven: 0,
            ambiguous: 0,
            unresolved: 0,
            external: 0,
            candidate_count_histogram: BTreeMap::new(),
            strategy_counts: BTreeMap::new(),
            proof_counts: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolutionQualityReport {
    pub call_sites: usize,
    pub resolved_exact: usize,
    pub resolved_high: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub legacy_only: usize,
    pub semantic_only: usize,
    pub disagreement: usize,
    #[serde(default)]
    pub relationship_telemetry: Vec<RelationshipResolutionTelemetry>,
}

impl ResolutionQualityReport {
    fn bucket_mut(
        &mut self,
        relationship: GraphEdgeType,
        language: open_kioku_core::Language,
    ) -> &mut RelationshipResolutionTelemetry {
        if let Some(index) = self.relationship_telemetry.iter().position(|bucket| {
            bucket.relationship == relationship && bucket.language == language
        }) {
            return &mut self.relationship_telemetry[index];
        }
        self.relationship_telemetry
            .push(RelationshipResolutionTelemetry::new(relationship, language));
        self.relationship_telemetry
            .last_mut()
            .expect("telemetry bucket was just inserted")
    }

    fn record_outcome(
        &mut self,
        relationship: GraphEdgeType,
        language: open_kioku_core::Language,
        outcome: &open_kioku_resolution::ResolutionOutcome,
    ) {
        let (candidate_count, status, candidates): (
            usize,
            &'static str,
            Vec<&open_kioku_resolution::ResolutionCandidate>,
        ) = match outcome {
            open_kioku_resolution::ResolutionOutcome::Proven { candidate } => {
                (1, "proven", vec![candidate])
            }
            open_kioku_resolution::ResolutionOutcome::Ambiguous { candidates, .. } => {
                (candidates.len(), "ambiguous", candidates.iter().collect())
            }
            open_kioku_resolution::ResolutionOutcome::Unresolved { candidates, .. } => {
                (candidates.len(), "unresolved", candidates.iter().collect())
            }
            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                (0, "external", Vec::new())
            }
        };
        let bucket = self.bucket_mut(relationship, language);
        bucket.cases += 1;
        bucket.candidates_considered += candidate_count;
        *bucket
            .candidate_count_histogram
            .entry(candidate_count.to_string())
            .or_default() += 1;
        match status {
            "proven" => bucket.proven += 1,
            "ambiguous" => bucket.ambiguous += 1,
            "unresolved" => bucket.unresolved += 1,
            "external" => bucket.external += 1,
            _ => unreachable!("internal telemetry status is closed"),
        }
        for candidate in candidates {
            for strategy in &candidate.strategies {
                *bucket
                    .strategy_counts
                    .entry(strategy.as_str().to_string())
                    .or_default() += 1;
            }
            for proof in &candidate.proofs {
                *bucket
                    .proof_counts
                    .entry(proof.kind.as_str().to_string())
                    .or_default() += 1;
            }
        }
    }

    fn record_inheritance(
        &mut self,
        relationship: GraphEdgeType,
        language: open_kioku_core::Language,
        edge: &open_kioku_resolution::inheritance::InheritanceEdge,
    ) {
        let candidate_count = edge.binding_candidate_count;
        let bucket = self.bucket_mut(relationship.clone(), language);
        bucket.cases += 1;
        bucket.candidates_considered += candidate_count;
        *bucket
            .candidate_count_histogram
            .entry(candidate_count.to_string())
            .or_default() += 1;
        if let Some(strategy) = edge.binding_strategy {
            *bucket
                .strategy_counts
                .entry(strategy.as_str().to_string())
                .or_default() += 1;
        }
        if edge.parent_id.is_some() && candidate_count == 1 {
            bucket.proven += 1;
            *bucket
                .proof_counts
                .entry(open_kioku_core::RelationshipProofKind::InheritanceBinding.as_str().into())
                .or_default() += 1;
            if let Some(strategy) = edge.binding_strategy {
                let kind = match strategy {
                    open_kioku_resolution::TypeDiscovery::SameFile => {
                        open_kioku_core::RelationshipProofKind::SameScopeDefinition
                    }
                    open_kioku_resolution::TypeDiscovery::ImportBinding => {
                        open_kioku_core::RelationshipProofKind::ImportBinding
                    }
                    open_kioku_resolution::TypeDiscovery::QualifiedName => {
                        open_kioku_core::RelationshipProofKind::QualifiedName
                    }
                };
                *bucket.proof_counts.entry(kind.as_str().into()).or_default() += 1;
            }
            if relationship == GraphEdgeType::Implements {
                *bucket
                    .proof_counts
                    .entry(
                        open_kioku_core::RelationshipProofKind::TraitOrInterfaceBinding
                            .as_str()
                            .into(),
                    )
                    .or_default() += 1;
            }
        } else if candidate_count > 1 {
            bucket.ambiguous += 1;
        } else {
            bucket.unresolved += 1;
        }
    }

    fn record_reference_occurrence(
        &mut self,
        language: open_kioku_core::Language,
        exact: bool,
    ) {
        let bucket = self.bucket_mut(GraphEdgeType::References, language);
        bucket.cases += 1;
        bucket.candidates_considered += 1;
        *bucket.candidate_count_histogram.entry("1".into()).or_default() += 1;
        if exact {
            bucket.proven += 1;
            *bucket.strategy_counts.entry("exact_occurrence".into()).or_default() += 1;
            *bucket
                .proof_counts
                .entry(open_kioku_core::RelationshipProofKind::ExactOccurrence.as_str().into())
                .or_default() += 1;
        } else {
            bucket.unresolved += 1;
            *bucket.strategy_counts.entry("heuristic".into()).or_default() += 1;
        }
    }

    fn normalize_telemetry(&mut self) {
        self.relationship_telemetry.sort_by(|left, right| {
            format!("{:?}", left.relationship)
                .cmp(&format!("{:?}", right.relationship))
                .then_with(|| {
                    format!("{:?}", left.language).cmp(&format!("{:?}", right.language))
                })
        });
    }
}
'''
text = replace_exact(text, old, new, "resolution telemetry model")

# Record every inheritance declaration, not only emitted ones, then preserve the existing proven-only
# structural emission behavior.
old = '''            for edge in inheritance_index.resolved_edges() {
                let Some(parent_id) = edge.parent_id.clone() else {
                    continue;
                };
                let Some(child_symbol) = symbol_index.get(&edge.child) else {
                    continue;
                };
                let Some(parent_symbol) = symbol_index.get(&parent_id) else {
                    continue;
                };
                let Some(file) = file_lookup.get(&child_symbol.file_id) else {
                    continue;
                };
                let edge_type = match edge.kind {
'''
new = '''            for edge in inheritance_index.all_edges() {
                let Some(child_symbol) = symbol_index.get(&edge.child) else {
                    continue;
                };
                let Some(file) = file_lookup.get(&child_symbol.file_id) else {
                    continue;
                };
                let edge_type = match edge.kind {
'''
text = replace_exact(text, old, new, "inheritance telemetry loop")
old = '''                    open_kioku_core::InheritanceKind::Embeds => continue,
                };
                if edge_type == GraphEdgeType::Implements
'''
new = '''                    open_kioku_core::InheritanceKind::Embeds => continue,
                };
                quality_report.record_inheritance(
                    edge_type.clone(),
                    child_symbol.language.clone(),
                    edge,
                );
                let Some(parent_id) = edge.parent_id.clone() else {
                    continue;
                };
                let Some(parent_symbol) = symbol_index.get(&parent_id) else {
                    continue;
                };
                if edge_type == GraphEdgeType::Implements
'''
text = replace_exact(text, old, new, "inheritance telemetry recording")

# Record declared-type outcomes before deciding whether a structural edge may be emitted.
old = '''                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } =
                    open_kioku_resolution::resolve_declared_type_use(
                        binding,
                        &source_symbol_id,
                        &file.path,
                        &semantic_repo,
                        &symbol_index,
                    )
                {
                    resolved_relationships.push(open_kioku_resolution::ResolvedRelationship {
'''
new = '''                let outcome = open_kioku_resolution::resolve_declared_type_use(
                    binding,
                    &source_symbol_id,
                    &file.path,
                    &semantic_repo,
                    &symbol_index,
                );
                quality_report.record_outcome(
                    GraphEdgeType::UsesType,
                    file.language.clone(),
                    &outcome,
                );
                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } = outcome {
                    resolved_relationships.push(open_kioku_resolution::ResolvedRelationship {
'''
text = replace_exact(text, old, new, "declared type telemetry recording")

# Record every call outcome before legacy comparison.
old = '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        let semantic_target = match &v2_outcome {
'''
new = '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        quality_report.record_outcome(
                            GraphEdgeType::Calls,
                            file.language.clone(),
                            &v2_outcome,
                        );
                        let semantic_target = match &v2_outcome {
'''
text = replace_exact(text, old, new, "call telemetry recording")

# References are finalized only after optional SCIP import, so telemetry runs over the final occurrence set.
anchor = '''        let repository = Repository {
'''
insert = '''        if resolution_mode != open_kioku_config::ResolutionMode::Legacy {
            for occurrence in occurrences.iter().filter(|occurrence| !occurrence.is_definition) {
                if let Some(file) = file_lookup.get(&occurrence.file_id) {
                    quality_report.record_reference_occurrence(
                        file.language.clone(),
                        occurrence.confidence == Confidence::Exact,
                    );
                }
            }
            quality_report.normalize_telemetry();
        }

'''
text = replace_exact(text, anchor, insert + anchor, "reference telemetry finalization")

# Small deterministic telemetry unit contract near the top-level types.
anchor = '''#[derive(Debug, Clone)]
pub struct IndexProgress {
'''
tests = '''#[cfg(test)]
mod resolution_quality_telemetry_tests {
    use super::*;
    use open_kioku_core::{RelationshipProof, RelationshipProofKind};

    #[test]
    fn telemetry_is_deterministic_and_keeps_authority_separate_from_confidence() {
        let target = SymbolId::new("symbol:target");
        let mut candidate = open_kioku_resolution::ResolutionCandidate::new(
            target.clone(),
            Confidence::Exact,
        )
        .with_strategy(open_kioku_resolution::ResolutionStrategy::TypedReceiver);
        let mut proof = RelationshipProof::new(
            RelationshipProofKind::QualifiedName,
            "qualified_name",
            1,
        );
        proof.target_symbol_id = Some(target);
        candidate.proofs.push(proof);
        let outcome = open_kioku_resolution::ResolutionOutcome::Unresolved {
            candidates: vec![candidate],
            reason: "not enough CALLS proof".into(),
        };

        let mut report = ResolutionQualityReport::default();
        report.record_outcome(
            GraphEdgeType::Calls,
            open_kioku_core::Language::Java,
            &outcome,
        );
        report.record_reference_occurrence(open_kioku_core::Language::Rust, true);
        report.normalize_telemetry();

        assert_eq!(report.relationship_telemetry.len(), 2);
        let calls = report
            .relationship_telemetry
            .iter()
            .find(|bucket| bucket.relationship == GraphEdgeType::Calls)
            .unwrap();
        assert_eq!(calls.unresolved, 1);
        assert_eq!(calls.proven, 0);
        assert_eq!(calls.candidate_count_histogram.get("1"), Some(&1));
        assert_eq!(calls.strategy_counts.get("typed_receiver"), Some(&1));
        assert_eq!(calls.proof_counts.get("qualified_name"), Some(&1));

        let first = serde_json::to_string(&report).unwrap();
        report.normalize_telemetry();
        let second = serde_json::to_string(&report).unwrap();
        assert_eq!(first, second);
    }
}

'''
text = replace_exact(text, anchor, tests + anchor, "telemetry unit tests")
ingest.write_text(text)
