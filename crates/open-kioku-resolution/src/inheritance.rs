use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use crate::type_candidates::{discover_type_candidates, TypeDiscovery};
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, InheritanceKind,
    InheritanceSite, LineRange, SourceRange, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::{BTreeMap, BTreeSet, HashMap};

fn call_file_range(call: &CallSite, ctx: &ResolutionContext<'_>) -> Option<FileRange> {
    Some(FileRange {
        path: ctx.file_path.to_path_buf(),
        line_range: Some(LineRange {
            start: call.range.start_line,
            end: call.range.end_line,
        }),
    })
}

#[derive(Debug, Clone)]
pub struct InheritanceEdge {
    pub child: SymbolId,
    pub parent_name: String,
    pub parent_id: Option<SymbolId>,
    pub kind: InheritanceKind,
    pub order: u16,
    pub range: SourceRange,
    pub binding_strategy: Option<TypeDiscovery>,
    pub binding_candidate_count: usize,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Default)]
pub struct InheritanceIndex {
    pub edges_by_child: HashMap<SymbolId, Vec<InheritanceEdge>>,
}

impl InheritanceIndex {
    pub fn build(sites: Vec<InheritanceSite>) -> Self {
        let mut index = Self::default();
        for site in sites {
            let parents = split_top_level_parent_names(&site.parent_name);
            for (offset, parent_name) in parents.into_iter().enumerate() {
                let order_offset = u16::try_from(offset).unwrap_or(u16::MAX);
                let order = site.order.saturating_add(order_offset);
                index
                    .edges_by_child
                    .entry(site.child_symbol_id.clone())
                    .or_default()
                    .push(InheritanceEdge {
                        child: site.child_symbol_id.clone(),
                        parent_name: parent_name.clone(),
                        parent_id: None,
                        kind: site.kind,
                        order,
                        range: site.range.clone(),
                        binding_strategy: None,
                        binding_candidate_count: 0,
                        evidence: vec![EvidenceId::new(format!(
                            "inheritance:{}:{:?}:{order}:{parent_name}",
                            site.child_symbol_id.0, site.kind
                        ))],
                    });
            }
        }
        for edges in index.edges_by_child.values_mut() {
            edges.sort_by(inheritance_edge_order);
        }
        index
    }

    /// Resolves parent names through the same deterministic type-candidate contract used by calls
    /// and declared type uses. Resolution is precedence-aware and fail-closed: ambiguity at a
    /// stronger scoped strategy does not fall through to a weaker strategy.
    pub fn bind_parents_with_repository(
        &mut self,
        symbols: &SymbolIndex,
        repository: &SemanticRepository,
    ) {
        for (child_id, edges) in self.edges_by_child.iter_mut() {
            let Some(child_sym) = symbols.get(child_id) else {
                continue;
            };

            for edge in edges {
                edge.parent_id = None;
                edge.binding_strategy = None;
                edge.binding_candidate_count = 0;

                let discovered = discover_type_candidates(
                    &child_sym.file_id,
                    child_sym.scope_id.as_ref(),
                    &edge.parent_name,
                    repository,
                    symbols,
                );
                for strategy in [
                    TypeDiscovery::SameFile,
                    TypeDiscovery::ImportBinding,
                    TypeDiscovery::QualifiedName,
                ] {
                    let candidates = discovered
                        .iter()
                        .filter(|candidate| candidate.discoveries.contains(&strategy))
                        .map(|candidate| candidate.target.clone())
                        .collect::<Vec<_>>();
                    if candidates.is_empty() {
                        continue;
                    }
                    edge.binding_strategy = Some(strategy);
                    edge.binding_candidate_count = candidates.len();
                    if candidates.len() == 1 {
                        edge.parent_id = candidates.first().cloned();
                    }
                    break;
                }
            }
        }
    }

    /// Deterministic view of uniquely bound inheritance declarations for structural emission.
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

    /// Returns every viable inherited member at the nearest inheritance depth containing a match.
    ///
    /// This is intentionally conservative for multiple inheritance: if two nearest parents expose
    /// the same member and language-specific ordering is not proven, both candidates survive and
    /// the caller must report ambiguity rather than using queue/hash insertion order as truth.
    pub fn inherited_member_candidates(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Vec<SymbolId> {
        let mut frontier = self.parent_ids(child_type_id);
        let mut visited = BTreeSet::<String>::new();

        while !frontier.is_empty() {
            let mut level_targets = BTreeMap::<String, SymbolId>::new();
            let mut next_frontier = BTreeMap::<String, SymbolId>::new();

            for parent_id in frontier {
                if !visited.insert(parent_id.0.clone()) {
                    continue;
                }
                if let Some(members) = symbols.by_parent.get(&parent_id) {
                    for target in members {
                        if symbols
                            .get(target)
                            .map(|symbol| {
                                symbol.name == member_name
                                    && matches!(
                                        symbol.kind,
                                        SymbolKind::Method
                                            | SymbolKind::Function
                                            | SymbolKind::Field
                                    )
                            })
                            .unwrap_or(false)
                        {
                            level_targets.insert(target.0.clone(), target.clone());
                        }
                    }
                }
                for next in self.parent_ids(&parent_id) {
                    if !visited.contains(&next.0) {
                        next_frontier.insert(next.0.clone(), next);
                    }
                }
            }

            if !level_targets.is_empty() {
                return level_targets.into_values().collect();
            }
            frontier = next_frontier.into_values().collect();
        }

        Vec::new()
    }

    /// Compatibility helper. A structural target is returned only when nearest-depth inheritance
    /// discovery produces exactly one candidate.
    pub fn resolve_inherited_member(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Option<SymbolId> {
        let candidates = self.inherited_member_candidates(child_type_id, member_name, symbols);
        (candidates.len() == 1).then(|| candidates[0].clone())
    }

    fn parent_ids(&self, child_type_id: &SymbolId) -> Vec<SymbolId> {
        let mut parents = self
            .edges_by_child
            .get(child_type_id)
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.parent_id.clone())
            .collect::<Vec<_>>();
        parents.sort_by(|left, right| left.0.cmp(&right.0));
        parents.dedup();
        parents
    }
}

fn inheritance_edge_order(left: &InheritanceEdge, right: &InheritanceEdge) -> std::cmp::Ordering {
    left.child
        .0
        .cmp(&right.child.0)
        .then_with(|| inheritance_kind_order(&left.kind).cmp(&inheritance_kind_order(&right.kind)))
        .then_with(|| left.order.cmp(&right.order))
        .then_with(|| left.parent_name.cmp(&right.parent_name))
}

fn inheritance_kind_order(kind: &InheritanceKind) -> u8 {
    match kind {
        InheritanceKind::Extends => 0,
        InheritanceKind::Implements => 1,
        InheritanceKind::TraitImpl => 2,
        InheritanceKind::Embeds => 3,
    }
}

/// Split comma-separated parent clauses only at top level. Generic argument commas, function-type
/// punctuation, and nested tuple/list syntax remain within the same parent expression.
fn split_top_level_parent_names(raw: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut angle = 0i32;
    let mut paren = 0i32;
    let mut square = 0i32;
    let mut brace = 0i32;
    for (index, ch) in raw.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = (angle - 1).max(0),
            '(' => paren += 1,
            ')' => paren = (paren - 1).max(0),
            '[' => square += 1,
            ']' => square = (square - 1).max(0),
            '{' => brace += 1,
            '}' => brace = (brace - 1).max(0),
            ',' if angle == 0 && paren == 0 && square == 0 && brace == 0 => {
                let value = raw[start..index].trim();
                if !value.is_empty() {
                    result.push(value.to_string());
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let value = raw[start..].trim();
    if !value.is_empty() {
        result.push(value.to_string());
    }
    result
}

pub fn resolve_super_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    if let Some(caller_id) = &call.caller_symbol_id {
        if let Some(caller_symbol) = ctx.symbols.get(caller_id) {
            if let Some(parent_type_id) = &caller_symbol.parent_symbol_id {
                let candidates = ctx.inheritance.inherited_member_candidates(
                    parent_type_id,
                    &call.callee_name,
                    ctx.symbols,
                );
                if candidates.len() == 1 {
                    let target = candidates[0].clone();
                    return ResolutionResult::Resolved {
                        target: target.clone(),
                        confidence: Confidence::Exact,
                        evidence: vec![ResolutionEvidence {
                            kind: ResolutionEvidenceKind::InheritanceGraph,
                            source_type: EvidenceSourceType::TreeSitter,
                            file_range: call_file_range(call, ctx),
                            symbol_id: Some(target),
                            message: "resolved super call via unique nearest inheritance target"
                                .into(),
                        }],
                    };
                }
                if candidates.len() > 1 {
                    return ResolutionResult::Ambiguous {
                        candidates,
                        reason: "multiple nearest inherited members match super call".into(),
                        evidence: vec![],
                    };
                }
            }
        }
    }

    ResolutionResult::Unresolved {
        reason: UnresolvedReason::NoCandidate,
        evidence: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, LineRange, ScopeId, Symbol, Visibility,
    };

    fn range(line: u32) -> SourceRange {
        SourceRange {
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 20,
        }
    }

    fn symbol_in_file(
        id: &str,
        name: &str,
        kind: SymbolKind,
        parent: Option<&str>,
        file: &str,
    ) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{name}"),
            kind,
            file_id: FileId::new(file),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Java,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: parent.map(SymbolId::new),
            scope_id: Some(ScopeId::new(format!("scope:{id}"))),
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn symbol(id: &str, name: &str, kind: SymbolKind, parent: Option<&str>) -> Symbol {
        symbol_in_file(id, name, kind, parent, &format!("file:{id}"))
    }

    fn bound_edge(child: &str, parent: &str, order: u16) -> InheritanceEdge {
        InheritanceEdge {
            child: SymbolId::new(child),
            parent_name: parent.into(),
            parent_id: Some(SymbolId::new(parent)),
            kind: InheritanceKind::Extends,
            order,
            range: range(1),
            binding_strategy: Some(TypeDiscovery::SameFile),
            binding_candidate_count: 1,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn build_splits_only_top_level_parent_commas() {
        let index = InheritanceIndex::build(vec![InheritanceSite {
            child_symbol_id: SymbolId::new("Child"),
            parent_name: "Alpha, Beta<Map<Key, Value>>, Gamma".into(),
            kind: InheritanceKind::Implements,
            order: 0,
            range: range(8),
        }]);
        let edges = index.edges_by_child.get(&SymbolId::new("Child")).unwrap();
        assert_eq!(
            edges
                .iter()
                .map(|edge| edge.parent_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta<Map<Key, Value>>", "Gamma"]
        );
        assert_eq!(
            edges.iter().map(|edge| edge.order).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn unique_same_file_parent_records_binding_strategy_and_range() {
        let file = "file:types";
        let child = symbol_in_file("Child", "Child", SymbolKind::Class, None, file);
        let parent = symbol_in_file("Base", "Base", SymbolKind::Class, None, file);
        let symbols = SymbolIndex::build(vec![child, parent]);
        let repository = SemanticRepository::new();
        let mut index = InheritanceIndex::build(vec![InheritanceSite {
            child_symbol_id: SymbolId::new("Child"),
            parent_name: "Base".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(12),
        }]);

        index.bind_parents_with_repository(&symbols, &repository);
        let resolved = index.resolved_edges();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].parent_id, Some(SymbolId::new("Base")));
        assert_eq!(resolved[0].binding_strategy, Some(TypeDiscovery::SameFile));
        assert_eq!(resolved[0].binding_candidate_count, 1);
        assert_eq!(resolved[0].range.start_line, 12);
    }

    #[test]
    fn ambiguous_same_file_parent_does_not_fall_through() {
        let file = "file:types";
        let child = symbol_in_file("Child", "Child", SymbolKind::Class, None, file);
        let first = symbol_in_file("BaseA", "Base", SymbolKind::Class, None, file);
        let second = symbol_in_file("BaseB", "Base", SymbolKind::Class, None, file);
        let symbols = SymbolIndex::build(vec![child, first, second]);
        let repository = SemanticRepository::new();
        let mut index = InheritanceIndex::build(vec![InheritanceSite {
            child_symbol_id: SymbolId::new("Child"),
            parent_name: "Base".into(),
            kind: InheritanceKind::Extends,
            order: 0,
            range: range(12),
        }]);

        index.bind_parents_with_repository(&symbols, &repository);
        let edge = &index.edges_by_child[&SymbolId::new("Child")][0];
        assert_eq!(edge.parent_id, None);
        assert_eq!(edge.binding_strategy, Some(TypeDiscovery::SameFile));
        assert_eq!(edge.binding_candidate_count, 2);
        assert!(index.resolved_edges().is_empty());
    }

    #[test]
    fn nearest_inherited_candidates_are_sorted_and_not_first_hit() {
        let symbols = SymbolIndex::build(vec![
            symbol("ParentA", "ParentA", SymbolKind::Class, None),
            symbol("ParentB", "ParentB", SymbolKind::Class, None),
            symbol("ParentA.run", "run", SymbolKind::Method, Some("ParentA")),
            symbol("ParentB.run", "run", SymbolKind::Method, Some("ParentB")),
        ]);
        let mut index = InheritanceIndex::default();
        index.edges_by_child.insert(
            SymbolId::new("Child"),
            vec![
                bound_edge("Child", "ParentB", 1),
                bound_edge("Child", "ParentA", 0),
            ],
        );

        let candidates =
            index.inherited_member_candidates(&SymbolId::new("Child"), "run", &symbols);
        assert_eq!(
            candidates
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["ParentA.run", "ParentB.run"]
        );
        assert_eq!(
            index.resolve_inherited_member(&SymbolId::new("Child"), "run", &symbols),
            None
        );
    }

    #[test]
    fn inheritance_walk_stops_at_nearest_depth_with_matches() {
        let symbols = SymbolIndex::build(vec![
            symbol("Parent", "Parent", SymbolKind::Class, None),
            symbol("Grand", "Grand", SymbolKind::Class, None),
            symbol("Parent.run", "run", SymbolKind::Method, Some("Parent")),
            symbol("Grand.run", "run", SymbolKind::Method, Some("Grand")),
        ]);
        let mut index = InheritanceIndex::default();
        index.edges_by_child.insert(
            SymbolId::new("Child"),
            vec![bound_edge("Child", "Parent", 0)],
        );
        index.edges_by_child.insert(
            SymbolId::new("Parent"),
            vec![bound_edge("Parent", "Grand", 0)],
        );

        assert_eq!(
            index.inherited_member_candidates(&SymbolId::new("Child"), "run", &symbols),
            vec![SymbolId::new("Parent.run")]
        );
    }
}
