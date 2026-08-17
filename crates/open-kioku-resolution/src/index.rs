use open_kioku_core::{Binding, FileId, ModuleId, Scope, ScopeId, SourceRange, Symbol, SymbolId};
use smallvec::SmallVec;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub by_id: HashMap<SymbolId, Symbol>,
    pub by_name: HashMap<String, SmallVec<[SymbolId; 4]>>,
    pub by_qualified: HashMap<String, SmallVec<[SymbolId; 2]>>,
    pub by_file: HashMap<FileId, Vec<SymbolId>>,
    pub by_module: HashMap<ModuleId, Vec<SymbolId>>,
    pub by_parent: HashMap<SymbolId, Vec<SymbolId>>,
}

impl SymbolIndex {
    pub fn build(symbols: Vec<Symbol>) -> Self {
        let mut index = Self::default();
        for sym in symbols {
            index
                .by_name
                .entry(sym.name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_qualified
                .entry(sym.qualified_name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_file
                .entry(sym.file_id.clone())
                .or_default()
                .push(sym.id.clone());
            if let Some(mod_id) = &sym.module_id {
                index
                    .by_module
                    .entry(mod_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            if let Some(parent_id) = &sym.parent_symbol_id {
                index
                    .by_parent
                    .entry(parent_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            index.by_id.insert(sym.id.clone(), sym);
        }
        for values in index.by_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_qualified.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_module.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_parent.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        index
    }

    pub fn get(&self, id: &SymbolId) -> Option<&Symbol> {
        self.by_id.get(id)
    }

    pub fn lookup_name(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScopeIndex {
    pub scopes: HashMap<ScopeId, Scope>,
}

impl ScopeIndex {
    pub fn build(scopes: Vec<Scope>) -> Self {
        let mut index = Self::default();
        for scope in scopes {
            index.scopes.insert(scope.id.clone(), scope);
        }
        index
    }

    pub fn get(&self, id: &ScopeId) -> Option<&Scope> {
        self.scopes.get(id)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BindingIndex {
    pub bindings_by_scope_name: HashMap<(ScopeId, String), Vec<Binding>>,
}

impl BindingIndex {
    pub fn build(bindings: Vec<Binding>) -> Self {
        let mut index = Self::default();
        for binding in bindings {
            index
                .bindings_by_scope_name
                .entry((binding.scope_id.clone(), binding.name.clone()))
                .or_default()
                .push(binding);
        }
        for values in index.bindings_by_scope_name.values_mut() {
            values.sort_by(|left, right| {
                (
                    left.range.start_line,
                    left.range.start_column,
                    left.range.end_line,
                    left.range.end_column,
                )
                    .cmp(&(
                        right.range.start_line,
                        right.range.start_column,
                        right.range.end_line,
                        right.range.end_column,
                    ))
                    .then_with(|| left.id.0.cmp(&right.id.0))
            });
        }
        index
    }

    pub fn resolve_before(
        &self,
        scope_id: &ScopeId,
        name: &str,
        call_range: &SourceRange,
        scopes: &ScopeIndex,
    ) -> Option<&Binding> {
        let mut current = Some(scope_id.clone());
        let mut visited = std::collections::HashSet::new();
        let call_pos = (call_range.start_line, call_range.start_column);
        while let Some(sid) = current {
            if !visited.insert(sid.clone()) {
                break;
            }
            if let Some(list) = self
                .bindings_by_scope_name
                .get(&(sid.clone(), name.to_string()))
            {
                if let Some(binding) = list.iter().rev().find(|binding| {
                    (binding.range.start_line, binding.range.start_column) <= call_pos
                }) {
                    return Some(binding);
                }
            }
            current = scopes.get(&sid).and_then(|scope| scope.parent_id.clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        BindingId, Confidence, EvidenceSourceType, Language, LineRange, SymbolKind, Visibility,
    };

    fn symbol(id: &str, name: &str, file: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
        }
    }

    #[test]
    fn symbol_candidate_indexes_do_not_depend_on_insertion_order() {
        let first = symbol("symbol:a", "run", "file:lib.rs");
        let second = symbol("symbol:b", "run", "file:lib.rs");

        let forward = SymbolIndex::build(vec![first.clone(), second.clone()]);
        let reversed = SymbolIndex::build(vec![second, first]);

        assert_eq!(forward.lookup_name("run"), reversed.lookup_name("run"));
        assert_eq!(
            forward.by_file.get(&FileId::new("file:lib.rs")),
            reversed.by_file.get(&FileId::new("file:lib.rs"))
        );
        assert_eq!(
            forward
                .lookup_name("run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
    }

    #[test]
    fn binding_ties_have_a_deterministic_order() {
        let scope = ScopeId::new("scope:file");
        let range = SourceRange {
            start_line: 4,
            start_column: 2,
            end_line: 4,
            end_column: 8,
        };
        let make = |id: &str| Binding {
            id: BindingId::new(id),
            file_id: FileId::new("file:lib.rs"),
            scope_id: scope.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: None,
            range: range.clone(),
        };

        let index = BindingIndex::build(vec![make("binding:z"), make("binding:a")]);
        let ids = index
            .bindings_by_scope_name
            .get(&(scope, "value".into()))
            .unwrap()
            .iter()
            .map(|binding| binding.id.0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["binding:a", "binding:z"]);
    }
}
