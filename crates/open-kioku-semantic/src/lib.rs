use open_kioku_config::SemanticConfig;
use open_kioku_core::{CodeChunk, File, SearchResult, Symbol};
use open_kioku_embeddings::{EmbeddingProvider, LocalHashEmbeddingProvider};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::MetadataStore;

pub struct SemanticSearchEngine<'a> {
    store: &'a dyn MetadataStore,
    provider: Box<dyn EmbeddingProvider>,
}

impl<'a> SemanticSearchEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore, provider: Box<dyn EmbeddingProvider>) -> Self {
        Self { store, provider }
    }

    pub fn from_config(
        store: &'a dyn MetadataStore,
        config: &SemanticConfig,
    ) -> Result<Option<Self>> {
        let Some(provider) = provider_from_config(config)? else {
            return Ok(None);
        };
        Ok(Some(Self::new(store, provider)))
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let files = self.store.list_files(usize::MAX, 0)?;
        let chunks = self.store.all_chunks()?;
        let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
        search_chunks_semantic(
            &chunks,
            &files,
            &symbols,
            query,
            limit,
            self.provider.as_ref(),
        )
    }
}

pub fn search_chunks_semantic(
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    query: &str,
    limit: usize,
    provider: &dyn EmbeddingProvider,
) -> Result<Vec<SearchResult>> {
    let query_vector = provider.embed(query)?;
    let mut scored = Vec::new();

    for chunk in chunks {
        let Some(file) = files.iter().find(|file| file.id == chunk.file_id) else {
            continue;
        };
        let chunk_vector = provider.embed(&chunk.text)?;
        let score = dot(&query_vector, &chunk_vector);
        if score <= 0.0 {
            continue;
        }
        let symbol = chunk
            .symbol_id
            .as_ref()
            .and_then(|id| symbols.iter().find(|symbol| symbol.id == *id))
            .cloned();
        scored.push(SearchResult {
            path: file.path.clone(),
            line_range: Some(chunk.range.clone()),
            snippet: snippet(&chunk.text),
            symbol,
            score,
            match_reason: "local hash embedding similarity".into(),
            evidence: vec![
                "query and chunk embedded locally with deterministic token hashing".into(),
                "no network or hosted embedding provider was used".into(),
            ],
            confidence: score.clamp(0.0, 1.0),
        });
    }

    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    scored.truncate(limit);
    Ok(scored)
}

pub fn provider_from_config(config: &SemanticConfig) -> Result<Option<Box<dyn EmbeddingProvider>>> {
    if !config.enabled {
        return Ok(None);
    }
    match config.provider.as_str() {
        "local" | "local-hash" | "hash" => {
            Ok(Some(Box::new(LocalHashEmbeddingProvider::default())))
        }
        "disabled" => Ok(None),
        other => Err(OkError::Unsupported(format!(
            "semantic provider `{other}` is not available; supported offline provider: local"
        ))),
    }
}

pub fn ensure_enabled(config: &SemanticConfig) -> Result<()> {
    provider_from_config(config).and_then(|provider| {
        provider
            .map(|_| ())
            .ok_or_else(|| OkError::Unsupported("semantic search is disabled in ok.toml".into()))
    })
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn snippet(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(240)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, SymbolId,
        SymbolKind,
    };
    use std::path::PathBuf;

    #[test]
    fn disabled_config_returns_no_provider() {
        let config = SemanticConfig {
            enabled: false,
            provider: "local".into(),
            model: String::new(),
        };

        assert!(provider_from_config(&config).unwrap().is_none());
    }

    #[test]
    fn unsupported_provider_is_explicit() {
        let config = SemanticConfig {
            enabled: true,
            provider: "remote-api".into(),
            model: String::new(),
        };

        let err = match provider_from_config(&config) {
            Ok(_) => panic!("unsupported provider should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not available"));
    }

    #[test]
    fn local_semantic_search_ranks_related_chunks() {
        let provider = LocalHashEmbeddingProvider::new(128).unwrap();
        let files = vec![
            file("file_auth", "src/auth.rs"),
            file("file_billing", "src/billing.rs"),
        ];
        let symbols = vec![symbol("symbol_issue_token", "issue_token", "file_auth")];
        let chunks = vec![
            chunk(
                "auth",
                "file_auth",
                "pub fn issue_token() { create session token }",
                Some("symbol_issue_token"),
            ),
            chunk(
                "billing",
                "file_billing",
                "pub fn invoice_total() { calculate invoice }",
                None,
            ),
        ];

        let results =
            search_chunks_semantic(&chunks, &files, &symbols, "issue token", 5, &provider).unwrap();

        assert_eq!(results[0].path, PathBuf::from("src/auth.rs"));
        assert_eq!(results[0].symbol.as_ref().unwrap().name, "issue_token");
        assert!(results[0]
            .evidence
            .iter()
            .any(|item| item.contains("no network")));
    }

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId(id.into()),
            repository_id: RepositoryId("repo".into()),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: String::new(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(id: &str, name: &str, file_id: &str) -> Symbol {
        Symbol {
            id: SymbolId(id.into()),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Function,
            file_id: FileId(file_id.into()),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn chunk(id: &str, file_id: &str, text: &str, symbol_id: Option<&str>) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file_id: FileId(file_id.into()),
            range: LineRange::single(1),
            language: Language::Rust,
            text: text.into(),
            symbol_id: symbol_id.map(|id| SymbolId(id.into())),
        }
    }
}
