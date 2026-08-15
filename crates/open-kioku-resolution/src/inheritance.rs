use open_kioku_core::{InheritanceKind, InheritanceSite, SymbolId};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct InheritanceEdge {
    pub child: SymbolId,
    pub parent_name: String,
    pub parent_id: Option<SymbolId>,
    pub kind: InheritanceKind,
    pub order: u16,
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
                });
        }
        index
    }
}
