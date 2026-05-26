use open_kioku_core::{Symbol, SymbolId};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::MetadataStore;

pub struct SymbolEngine<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> SymbolEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    pub fn definition(&self, query: &str) -> Result<Symbol> {
        self.store
            .list_symbols(Some(query), 1, 0)?
            .into_iter()
            .next()
            .ok_or_else(|| OkError::SymbolNotFound(query.into()))
    }

    pub fn references(&self, query: &str, limit: usize) -> Result<Vec<open_kioku_core::SymbolOccurrence>> {
        let symbol = self.definition(query)?;
        self.store.references_for_symbol(&symbol.id, limit)
    }
}
