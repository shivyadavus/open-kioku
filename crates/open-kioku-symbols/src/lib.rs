use open_kioku_core::{Symbol, SymbolId, SymbolOccurrence};
use open_kioku_errors::{OcfError, Result};
use open_kioku_storage::MetadataStore;

pub struct SymbolEngine<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> SymbolEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        self.store.list_symbols(Some(query), limit, 0)
    }

    pub fn definition(&self, query: &str) -> Result<Symbol> {
        let matches = self.find(query, 10)?;
        matches
            .into_iter()
            .find(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
            .ok_or_else(|| OcfError::SymbolNotFound(query.into()))
    }

    pub fn by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
        self.store.symbol_by_id(id)
    }

    pub fn references(&self, query: &str, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let symbol = self.definition(query)?;
        self.store.references_for_symbol(&symbol.id, limit)
    }
}
