use open_kioku_core::{
    Binding, FileId, ModuleId, Scope, ScopeId, SourceRange, Symbol, SymbolId,
};
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
            index.by_name.entry(sym.name.clone()).or_default().push(sym.id.clone());
            index.by_qualified.entry(sym.qualified_name.clone()).or_default().push(sym.id.clone());
            index.by_file.entry(sym.file_id.clone()).or_default().push(sym.id.clone());
            if let Some(mod_id) = &sym.module_id {
                index.by_module.entry(mod_id.clone()).or_default().push(sym.id.clone());
            }
            if let Some(parent_id) = &sym.parent_symbol_id {
                index.by_parent.entry(parent_id.clone()).or_default().push(sym.id.clone());
            }
            index.by_id.insert(sym.id.clone(), sym);
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
        for vec in index.bindings_by_scope_name.values_mut() {
            vec.sort_by_key(|b| b.range.start_line);
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
        while let Some(sid) = current {
            if let Some(list) = self.bindings_by_scope_name.get(&(sid.clone(), name.to_string())) {
                if let Some(b) = list.iter().rev().find(|b| b.range.start_line <= call_range.start_line) {
                    return Some(b);
                }
            }
            current = scopes.get(&sid).and_then(|s| s.parent_id.clone());
        }
        None
    }
}
