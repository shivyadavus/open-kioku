use open_kioku_core::SearchResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    pub include_generated: bool,
    pub include_vendor: bool,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 20,
            include_generated: false,
            include_vendor: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: SearchQuery,
    pub results: Vec<SearchResult>,
}
