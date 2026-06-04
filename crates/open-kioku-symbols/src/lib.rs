use open_kioku_core::{
    Confidence, EvidenceSourceType, Symbol, SymbolId, SymbolKind, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
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
        let mut matches = self
            .find(query, 250)?
            .into_iter()
            .filter(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
            .collect::<Vec<_>>();
        matches.sort_by_key(|symbol| definition_rank(symbol, query));
        matches
            .into_iter()
            .next()
            .ok_or_else(|| OkError::SymbolNotFound(query.into()))
    }

    pub fn by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
        self.store.symbol_by_id(id)
    }

    pub fn references(&self, query: &str, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let symbol = self.definition(query)?;
        let refs = self.store.references_for_symbol(&symbol.id, limit)?;
        if !refs.is_empty() {
            return Ok(refs);
        }
        self.lexical_references(&symbol, limit)
    }

    fn lexical_references(&self, symbol: &Symbol, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let name = &symbol.name;
        let mut occurrences = Vec::new();
        let chunks = self.store.find_chunks_containing(name, limit * 4)?;
        for chunk in chunks {
            if let Some(idx) = chunk.text.find(name) {
                let before_ok = idx == 0 || {
                    let prev_char = chunk.text[..idx].chars().next_back().unwrap();
                    !prev_char.is_alphanumeric() && prev_char != '_'
                };
                let after_ok = idx + name.len() == chunk.text.len() || {
                    let next_char = chunk.text[idx + name.len()..].chars().next().unwrap();
                    !next_char.is_alphanumeric() && next_char != '_'
                };
                if before_ok && after_ok {
                    occurrences.push(SymbolOccurrence {
                        symbol_id: symbol.id.clone(),
                        file_id: chunk.file_id.clone(),
                        range: Some(chunk.range.clone()),
                        is_definition: false,
                        confidence: Confidence::Low,
                        provenance: EvidenceSourceType::Lexical,
                    });
                    if occurrences.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(occurrences)
    }
}

fn definition_rank(symbol: &Symbol, query: &str) -> (u8, u8, usize) {
    let exactness = if symbol.name == query {
        0
    } else if symbol.qualified_name.ends_with(&format!("::{query}")) {
        1
    } else {
        2
    };
    (
        exactness,
        symbol_kind_rank(&symbol.kind),
        symbol.qualified_name.len(),
    )
}

fn symbol_kind_rank(kind: &SymbolKind) -> u8 {
    match kind {
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface => 0,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Endpoint => 1,
        SymbolKind::Module | SymbolKind::Package => 2,
        SymbolKind::Constant | SymbolKind::Field | SymbolKind::Variable => 3,
        SymbolKind::DatabaseTable | SymbolKind::Test | SymbolKind::Unknown => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolEngine;
    use open_kioku_core::{
        CodeChunk, File, FileId, Import, IndexManifest, Language, LineRange, Symbol, SymbolId,
        SymbolKind, SymbolOccurrence, TestTarget,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{IndexData, MetadataStore};
    use std::path::Path;

    #[derive(Default)]
    struct MemoryStore {
        symbols: Vec<Symbol>,
    }

    impl MetadataStore for MemoryStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, _limit: usize, _offset: usize) -> Result<Vec<File>> {
            Ok(Vec::new())
        }

        fn get_file_by_path(&self, _path: &Path) -> Result<Option<File>> {
            Ok(None)
        }

        fn list_symbols(
            &self,
            query: Option<&str>,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<Symbol>> {
            let query = query.unwrap_or_default();
            Ok(self
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.name.contains(query) || symbol.qualified_name.contains(query)
                })
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn symbol_by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(self.symbols.iter().find(|symbol| symbol.id == *id).cloned())
        }

        fn chunks_for_file(&self, _file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(Vec::new())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn references_for_symbol(
            &self,
            _id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }

        fn occurrences_for_file(&self, _file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }
    }

    fn symbol(id: &str, name: &str, qualified_name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified_name.into(),
            kind,
            file_id: FileId::new("file"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: open_kioku_core::Confidence::High,
            provenance: open_kioku_core::EvidenceSourceType::TreeSitter,
        }
    }

    #[test]
    fn definition_prefers_exact_class_match_over_prefix_matches() {
        let store = MemoryStore {
            symbols: vec![
                symbol(
                    "prefix",
                    "SearchServiceCleanupOnLostMasterIT",
                    "server::SearchServiceCleanupOnLostMasterIT",
                    SymbolKind::Class,
                ),
                symbol(
                    "field",
                    "searchService",
                    "server::TransportSearchAction::searchService",
                    SymbolKind::Field,
                ),
                symbol(
                    "class",
                    "SearchService",
                    "server::search::SearchService::SearchService",
                    SymbolKind::Class,
                ),
                symbol(
                    "ctor",
                    "SearchService",
                    "server::search::SearchService::SearchService",
                    SymbolKind::Method,
                ),
            ],
        };

        let definition = SymbolEngine::new(&store)
            .definition("SearchService")
            .unwrap();

        assert_eq!(definition.id.0, "class");
    }
}
