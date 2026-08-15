use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use crate::index::SymbolIndex;
use open_kioku_core::{
    CallSite, Confidence, EvidenceId, EvidenceSourceType, FileRange, InheritanceKind,
    InheritanceSite, LineRange, SymbolId, SymbolKind,
};
use open_kioku_semantic_model::SemanticRepository;
use std::collections::{HashMap, HashSet, VecDeque};

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
            edges.sort_by_key(|e| {
                let kind_order = match e.kind {
                    InheritanceKind::Extends => 0,
                    InheritanceKind::Implements => 1,
                    InheritanceKind::TraitImpl => 2,
                    InheritanceKind::Embeds => 3,
                };
                (kind_order, e.order)
            });
        }
        index
    }

    /// Resolves string parent names into SymbolIds with evidence (same file, imports, qualified name),
    /// not project-global unique matching.
    pub fn bind_parents_with_repository(
        &mut self,
        symbols: &SymbolIndex,
        repository: &SemanticRepository,
    ) {
        for (child_id, edges) in self.edges_by_child.iter_mut() {
            let child_sym = match symbols.get(child_id) {
                Some(s) => s,
                None => continue,
            };

            for edge in edges {
                let parent_name = &edge.parent_name;

                // 1. Same-file class/trait/interface match
                if let Some(file_symbols) = symbols.by_file.get(&child_sym.file_id) {
                    let matching: Vec<&SymbolId> = file_symbols
                        .iter()
                        .filter(|id| {
                            symbols
                                .get(id)
                                .map(|s| {
                                    s.name == *parent_name
                                        && matches!(
                                            s.kind,
                                            SymbolKind::Class
                                                | SymbolKind::Trait
                                                | SymbolKind::Interface
                                        )
                                })
                                .unwrap_or(false)
                        })
                        .collect();
                    if matching.len() == 1 {
                        edge.parent_id = Some(matching[0].clone());
                        continue;
                    }
                }

                // 2. Import binding lookup
                let import_bindings =
                    repository
                        .imports
                        .lookup(&child_sym.file_id, None, parent_name);
                for binding in import_bindings {
                    if let Some(target) = &binding.target_symbol {
                        edge.parent_id = Some(target.clone());
                        break;
                    }
                }
                if edge.parent_id.is_some() {
                    continue;
                }

                // 3. Qualified name match
                if let Some(qualified) = symbols.by_qualified.get(parent_name) {
                    if qualified.len() == 1 {
                        edge.parent_id = Some(qualified[0].clone());
                    }
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
        let mut queue = VecDeque::new();

        if let Some(edges) = self.edges_by_child.get(child_type_id) {
            for edge in edges {
                if let Some(pid) = &edge.parent_id {
                    queue.push_back(pid.clone());
                }
            }
        }

        while let Some(current_parent_id) = queue.pop_front() {
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
                        .map(|s| {
                            s.name == member_name
                                && matches!(
                                    s.kind,
                                    SymbolKind::Method | SymbolKind::Function | SymbolKind::Field
                                )
                        })
                        .unwrap_or(false)
                })
                .collect();

            if method_candidates.len() == 1 {
                return Some(method_candidates[0].clone());
            }

            if let Some(parent_edges) = self.edges_by_child.get(&current_parent_id) {
                for edge in parent_edges {
                    if let Some(pid) = &edge.parent_id {
                        queue.push_back(pid.clone());
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
