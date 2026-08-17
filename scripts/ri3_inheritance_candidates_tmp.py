from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

path = Path("crates/open-kioku-resolution/src/inheritance.rs")
text = path.read_text()

old_sort = '''        for edges in index.edges_by_child.values_mut() {
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
'''
new_sort = '''        for edges in index.edges_by_child.values_mut() {
            sort_inheritance_edges(edges);
        }
'''
text = replace_exact(text, old_sort, new_sort, "inheritance build ordering")

old_import_binding = '''                // 2. Import binding lookup
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
'''
new_import_binding = '''                // 2. Import binding lookup. Multiple exact bindings are ambiguous; never
                // choose the first binding based on repository insertion order.
                let import_bindings =
                    repository
                        .imports
                        .lookup(&child_sym.file_id, None, parent_name);
                let mut imported_targets = import_bindings
                    .iter()
                    .filter_map(|binding| binding.target_symbol.clone())
                    .collect::<Vec<_>>();
                imported_targets.sort_by(|left, right| left.0.cmp(&right.0));
                imported_targets.dedup();
                if imported_targets.len() == 1 {
                    edge.parent_id = imported_targets.pop();
                    continue;
                }
'''
text = replace_exact(text, old_import_binding, new_import_binding, "ambiguous inheritance import binding")

bind_end = '''            }
        }
    }

    /// Solves parent member resolution along inheritance chains (Java superclass, Python C3 MRO, Rust traits/inherent, Go embedding).
'''
bind_replacement = '''            }
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
'''
text = replace_exact(text, bind_end, bind_replacement, "inheritance candidate API insertion")

start = text.find('    pub fn resolve_inherited_member(\n')
end = text.find('\n    }\n}\n\npub fn resolve_super_member', start)
if start < 0 or end < 0:
    raise SystemExit(f"legacy inherited resolver anchors invalid: start={start}, end={end}")
legacy_fn = '''    pub fn resolve_inherited_member(
        &self,
        child_type_id: &SymbolId,
        member_name: &str,
        symbols: &SymbolIndex,
    ) -> Option<SymbolId> {
        self.inherited_member_candidates(child_type_id, member_name, symbols)
            .into_iter()
            .next()
'''
text = text[:start] + legacy_fn + text[end:]

helper_anchor = 'pub fn resolve_super_member(call: &CallSite, ctx: &ResolutionContext<\'_>) -> ResolutionResult {\n'
helpers = '''fn inheritance_kind_order(kind: &InheritanceKind) -> u8 {
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

'''
text = replace_exact(text, helper_anchor, helpers + helper_anchor, "inheritance helpers")

path.write_text(text)
