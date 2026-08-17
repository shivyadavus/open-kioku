use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, InheritanceKind,
    InheritanceSite, LineRange, SymbolId, SymbolKind,
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
            index
                .edges_by_child
                .entry(site.child_symbol_id.clone())
                .or_default()
                .push(InheritanceEdge {
                    child: site.child_symbol_id,
                    parent_name: site.parent_name,
                    parent_id: None,
                    kind: site.kind,
                    order: site.order,
                    evidence: Vec::new(),
                });
        }
        for edges in index.edges_by_child.values_mut() {
            edges.sort_by(|left, right| {
                inheritance_kind_order(&left.kind)
                    .cmp(&inheritance_kind_order(&right.kind))
                    .then_with(|| left.order.cmp(&right.order))
                    .then_with(|| left.parent_name.cmp(&right.parent_name))
                    .then_with(|| left.child.0.cmp(&right.child.0))
            });
        }
        index
    }

    /// Resolves string parent names into SymbolIds using only scoped structural evidence.
    /// Ambiguous imports remain unbound instead of accepting the first binding returned by storage.
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
                let parent_name = &edge.parent_name;

                let same_file = symbols
                    .by_file
                    .get(&child_sym.file_id)
                    .into_iter()
                    .flatten()
                    .filter(|id| is_named_parent_type(symbols, id, parent_name))
                    .cloned()
                    .collect::<Vec<_>>();
                if same_file.len() == 1 {
                    edge.parent_id = same_file.first().cloned();
                    continue;
                }
                if same_file.len() > 1 {
                    continue;
                }

                let mut imported = BTreeMap::<String, SymbolId>::new();
                for binding in repository
                    .imports
                    .lookup(&child_sym.file_id, None, parent_name)
                {
                    if let Some(target) = &binding.target_symbol {
                        if is_parent_type(symbols, target) {
                            imported.insert(target.0.clone(), target.clone());
                        }
                    }
                    if let Some(target_file) = &binding.target_file {
                        if let Some(file_symbols) = symbols.by_file.get(target_file) {
                            for target in file_symbols {
                                if is_named_parent_type(symbols, target, parent_name) {
                                    imported.insert(target.0.clone(), target.clone());
                                }
                            }
                        }
                    }
                }
                if imported.len() == 1 {
                    edge.parent_id = imported.into_values().next();
                    continue;
                }
                if imported.len() > 1 {
                    continue;
                }

                if let Some(qualified) = symbols.by_qualified.get(parent_name) {
                    let exact = qualified
                        .iter()
                        .filter(|id| is_parent_type(symbols, id))
                        .cloned()
                        .collect::<Vec<_>>();
                    if exact.len() == 1 {
                        edge.parent_id = exact.first().cloned();
                    }
                }
            }
        }
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

fn inheritance_kind_order(kind: &InheritanceKind) -> u8 {
    match kind {
        InheritanceKind::Extends => 0,
        InheritanceKind::Implements => 1,
        InheritanceKind::TraitImpl => 2,
        InheritanceKind::Embeds => 3,
    }
}

fn is_named_parent_type(symbols: &SymbolIndex, id: &SymbolId, name: &str) -> bool {
    symbols
        .get(id)
        .map(|symbol| symbol.name == name && is_parent_kind(&symbol.kind))
        .unwrap_or(false)
}

fn is_parent_type(symbols: &SymbolIndex, id: &SymbolId) -> bool {
    symbols
        .get(id)
        .map(|symbol| is_parent_kind(&symbol.kind))
        .unwrap_or(false)
}

fn is_parent_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Module
    )
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
        Confidence, EvidenceSourceType, FileId, Language, LineRange, Symbol, Visibility,
    };

    fn symbol(id: &str, name: &str, kind: SymbolKind, parent: Option<&str>) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind,
            file_id: FileId::new(format!("file:{id}")),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Java,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: parent.map(SymbolId::new),
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        }
    }

    fn bound_edge(child: &str, parent: &str, order: u16) -> InheritanceEdge {
        InheritanceEdge {
            child: SymbolId::new(child),
            parent_name: parent.into(),
            parent_id: Some(SymbolId::new(parent)),
            kind: InheritanceKind::Extends,
            order,
            evidence: Vec::new(),
        }
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
