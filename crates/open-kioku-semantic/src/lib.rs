use open_kioku_core::SearchResult;
use open_kioku_errors::{OcfError, Result};

pub trait SemanticSearch: Send + Sync {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
}

pub struct DisabledSemanticSearch;

impl SemanticSearch for DisabledSemanticSearch {
    fn search(&self, _query: &str, _limit: usize) -> Result<Vec<SearchResult>> {
        Err(OcfError::Unsupported(
            "semantic search is disabled; lexical, symbol, and graph evidence remain authoritative"
                .into(),
        ))
    }
}
