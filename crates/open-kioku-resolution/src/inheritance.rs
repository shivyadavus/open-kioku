use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, InheritanceKind,
    InheritanceSite, LineRange, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::{HashMap, HashSet};

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
            sort_inheritance_edges(edges);
        }
        index
    }

    /// Resolves string parent names through the same complete, deterministic candidate collector
    /// used by proof-gated inheritance relationship emission.
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
                let candidates = crate::type_relations::collect_parent_type_candidates(
                    child_sym,
                    &edge.parent_name,
                    symbols,
                    repository,
                );
                edge.parent_id = match candidates.as_slice() {
                    [candidate] => Some(candidate.target.clone()),
                    _ => None,
                };
            }
        }
        for edges in self.edges_by_child.values_mut() {
            sort_inheritance_edges(edges);
        }
    }

    /// Returns all matching members at the nearest inheritance depth in deterministic order.
    ///
    /// The generic inheritance layer intentionally does not choose among multiple parents at the
    /// same depth. Language-specific precedence/MRO belongs in semantic adapters; callers that need
    /// structural truth must evaluate the full candidate set.
    pub fn inherited_member_candidates(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Vec<SymbolId> {
        let mut visited = HashSet::new();
        let mut frontier = parent_ids(self.edges_by_child.get(child_type_id));

        while !frontier.is_empty() {
            let mut matches = Vec::new();
            let mut next = Vec::new();
            for parent_id in frontier {
                if !visited.insert(parent_id.clone()) {
                    continue;
                }
                if let Some(members) = symbols.by_parent.get(&parent_id) {
                    for id in members {
                        if symbols
                            .get(id)
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
                            matches.push(id.clone());
                        }
                    }
                }
                next.extend(parent_ids(self.edges_by_child.get(&parent_id)));
            }
            matches.sort_by(|left, right| left.0.cmp(&right.0));
            matches.dedup();
            if !matches.is_empty() {
                return matches;
            }
            next.sort_by(|left, right| left.0.cmp(&right.0));
            next.dedup();
            frontier = next;
        }
        Vec::new()
    }

    /// Compatibility helper for callers not yet migrated to proof-gated candidate evaluation.
    /// Structural resolvers must use `inherited_member_candidates` instead of trusting this winner.
    pub fn resolve_inherited_member(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Option<SymbolId> {
        self.inherited_member_candidates(child_type_id, member_name, symbols)
            .into_iter()
            .next()
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

fn sort_inheritance_edges(edges: &mut [InheritanceEdge]) {
    edges.sort_by(|left, right| {
        (
            inheritance_kind_order(&left.kind),
            left.order,
            &left.parent_name,
            left.parent_id.as_ref().map(|id| id.0.as_str()),
        )
            .cmp(&(
                inheritance_kind_order(&right.kind),
                right.order,
                &right.parent_name,
                right.parent_id.as_ref().map(|id| id.0.as_str()),
            ))
    });
}

fn parent_ids(edges: Option<&Vec<InheritanceEdge>>) -> Vec<SymbolId> {
    let mut ids = edges
        .into_iter()
        .flat_map(|edges| edges.iter())
        .filter_map(|edge| edge.parent_id.clone())
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.0.cmp(&right.0));
    ids.dedup();
    ids
}

pub fn resolve_super_member(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    if let Some(caller_id) = &call.caller_symbol_id {
        if let Some(caller_symbol) = ctx.symbols.get(caller_id) {
            if let Some(parent_type_id) = &caller_symbol.parent_symbol_id {
                if let Some(target) = ctx.inheritance.resolve_inherited_member(
                    parent_type_id,
                    &call.callee_name,
                    ctx.symbols,
                ) {
                    return ResolutionResult::Resolved {
                        target: target.clone(),
                        confidence: Confidence::Exact,
                        evidence: vec![ResolutionEvidence {
                            kind: ResolutionEvidenceKind::InheritanceGraph,
                            source_type: EvidenceSourceType::TreeSitter,
                            file_range: call_file_range(call, ctx),
                            symbol_id: Some(target),
                            message: "resolved super call via inheritance graph traversal".into(),
                        }],
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
