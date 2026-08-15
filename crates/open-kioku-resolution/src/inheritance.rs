use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, InheritanceKind, InheritanceSite,
    SymbolId,
};
use std::collections::{HashMap, HashSet};

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
        index
    }

    /// Resolves string parent names into SymbolIds against the SymbolIndex.
    pub fn bind_parents(&mut self, symbols: &SymbolIndex) {
        for edges in self.edges_by_child.values_mut() {
            for edge in edges {
                let candidates = symbols.lookup_name(&edge.parent_name);
                if candidates.len() == 1 {
                    edge.parent_id = Some(candidates[0].clone());
                }
            }
        }
    }

    /// Solves parent member resolution along inheritance chains (Java superclass, Python C3 MRO, Rust traits/inherent, Go embedding).
    pub fn resolve_inherited_member(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Option<SymbolId> {
        let mut visited = HashSet::new();
        let mut queue = Vec::new();

        if let Some(edges) = self.edges_by_child.get(child_type_id) {
            for edge in edges {
                if let Some(pid) = &edge.parent_id {
                    queue.push(pid.clone());
                }
            }
        }

        while let Some(current_parent_id) = queue.pop() {
            if !visited.insert(current_parent_id.clone()) {
                continue;
            }

            let method_candidates: Vec<SymbolId> = symbols
                .by_parent
                .get(&current_parent_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|id| {
                    symbols
                        .get(id)
                        .map(|s| s.name == member_name)
                        .unwrap_or(false)
                })
                .collect();

            if method_candidates.len() == 1 {
                return Some(method_candidates[0].clone());
            }

            if let Some(parent_edges) = self.edges_by_child.get(&current_parent_id) {
                for edge in parent_edges {
                    if let Some(pid) = &edge.parent_id {
                        queue.push(pid.clone());
                    }
                }
            }
        }

        None
    }
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
                            file_range: None,
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
