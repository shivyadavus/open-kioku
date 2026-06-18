use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, GraphNode, LineRange, ScoreComponent,
    SearchResult, Symbol,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::EvidenceBuilder;
use open_kioku_storage::SearchIndex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{doc, Index};

pub struct TantivySearchIndex {
    index: Index,
    fields: TantivyFields,
}

#[derive(Clone, Copy)]
struct TantivyFields {
    path: Field,
    content: Field,
    chunk_json: Field,
    file_json: Field,
    symbol_json: Field,
}

impl TantivySearchIndex {
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        fs::create_dir_all(path)?;
        let schema = schema();
        let index = match Index::open_in_dir(path) {
            Ok(index) => index,
            Err(_) => Index::create_in_dir(path, schema.clone()).map_err(search_err)?,
        };
        let fields = fields(index.schema())?;
        Ok(Self { index, fields })
    }

    pub fn exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().join("meta.json").exists()
    }
}

impl SearchIndex for TantivySearchIndex {
    fn rebuild(&mut self, chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) -> Result<()> {
        self.rebuild_with_graph(chunks, files, symbols, &[])
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_code(query, limit)
    }
}

impl TantivySearchIndex {
    pub fn rebuild_with_graph(
        &mut self,
        chunks: &[CodeChunk],
        files: &[File],
        symbols: &[Symbol],
        graph_nodes: &[GraphNode],
    ) -> Result<()> {
        let mut writer = self.index.writer(50_000_000).map_err(search_err)?;
        writer.delete_all_documents().map_err(search_err)?;
        let files_by_id = files
            .iter()
            .map(|file| (file.id.0.as_str(), file))
            .collect::<HashMap<_, _>>();
        let symbols_by_id = symbols
            .iter()
            .map(|symbol| (symbol.id.0.as_str(), symbol))
            .collect::<HashMap<_, _>>();
        for chunk in chunks {
            let Some(file) = files_by_id.get(chunk.file_id.0.as_str()) else {
                continue;
            };
            let symbol = chunk
                .symbol_id
                .as_ref()
                .and_then(|id| symbols_by_id.get(id.0.as_str()).copied());
            let symbol_json = symbol
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_default();
            writer
                .add_document(doc!(
                    self.fields.path => file.path.to_string_lossy().to_string(),
                    self.fields.content => format!("{}\n{}", file.path.display(), chunk.text),
                    self.fields.chunk_json => serde_json::to_string(chunk)?,
                    self.fields.file_json => serde_json::to_string(file)?,
                    self.fields.symbol_json => symbol_json,
                ))
                .map_err(search_err)?;
        }
        for node in graph_nodes {
            let file = node
                .file_id
                .as_ref()
                .and_then(|id| files_by_id.get(id.0.as_str()).copied())
                .or_else(|| {
                    node.symbol_id.as_ref().and_then(|id| {
                        symbols_by_id
                            .get(id.0.as_str())
                            .and_then(|symbol| files_by_id.get(symbol.file_id.0.as_str()).copied())
                    })
                });
            let symbol = node
                .symbol_id
                .as_ref()
                .and_then(|id| symbols_by_id.get(id.0.as_str()).copied());
            let Some(file) = file else {
                continue;
            };
            let symbol_json = symbol
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_default();
            let graph_chunk = CodeChunk {
                id: graph_chunk_id(node),
                file_id: file.id.clone(),
                range: symbol
                    .and_then(|symbol| symbol.range.clone())
                    .unwrap_or_else(|| LineRange::single(1)),
                language: file.language.clone(),
                text: graph_node_text(node, file, symbol),
                symbol_id: node.symbol_id.clone(),
            };
            writer
                .add_document(doc!(
                    self.fields.path => file.path.to_string_lossy().to_string(),
                    self.fields.content => graph_chunk.text.clone(),
                    self.fields.chunk_json => serde_json::to_string(&graph_chunk)?,
                    self.fields.file_json => serde_json::to_string(file)?,
                    self.fields.symbol_json => symbol_json,
                ))
                .map_err(search_err)?;
        }
        writer.commit().map_err(search_err)?;
        Ok(())
    }

    pub fn search_all(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_filtered(query, limit, SearchDocumentFilter::All)
    }

    pub fn search_code(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_filtered(query, limit, SearchDocumentFilter::CodeOnly)
    }

    pub fn search_graph(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_filtered(query, limit, SearchDocumentFilter::GraphOnly)
    }

    fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        filter: SearchDocumentFilter,
    ) -> Result<Vec<SearchResult>> {
        let raw_query = query.trim();
        if raw_query.is_empty() {
            return Ok(Vec::new());
        }
        let reader = self.index.reader().map_err(search_err)?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(
            &self.index,
            vec![
                self.fields.content,
                self.fields.path,
                self.fields.symbol_json,
            ],
        );
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for variant in query_variants(raw_query) {
            let Ok(query) = parser.parse_query(&variant) else {
                continue;
            };
            let top_docs = searcher
                .search(
                    &query,
                    &TopDocs::with_limit(limit.saturating_mul(4).max(limit)),
                )
                .map_err(search_err)?;
            for (score, address) in top_docs {
                let document: TantivyDocument = searcher.doc(address).map_err(search_err)?;
                let chunk: CodeChunk = required_json(&document, self.fields.chunk_json)?;
                let file: File = required_json(&document, self.fields.file_json)?;
                let is_graph = is_graph_chunk(&chunk);
                if filter == SearchDocumentFilter::GraphOnly && !is_graph {
                    continue;
                }
                if filter == SearchDocumentFilter::CodeOnly && is_graph {
                    continue;
                }
                let key = format!("{}:{}:{}", file.path.display(), chunk.range.start, chunk.id);
                if !seen.insert(key) {
                    continue;
                }
                let symbol: Option<Symbol> = optional_json(&document, self.fields.symbol_json)?;
                let boosted_score =
                    score + variant_boost(raw_query, &variant, &file, symbol.as_ref(), &chunk);
                let evidence_builder = EvidenceBuilder::new()
                    .add("BM25 lexical match from local Tantivy index", score)
                    .add(
                        format!("query variant `{variant}` matched local index"),
                        boosted_score,
                    );
                let evidence_builder = if is_graph {
                    evidence_builder.add(
                        format!("graph-node identifier document `{}` matched", chunk.id),
                        boosted_score + 0.3,
                    )
                } else {
                    evidence_builder
                };
                let (evidence_strings, confidence) = evidence_builder.build();
                let path = file.path;
                let line_range = if is_graph {
                    None
                } else {
                    Some(chunk.range.clone())
                };
                let evidence_ids = if is_graph {
                    vec![chunk.id.clone()]
                } else {
                    search_result_evidence_ids(&path, &line_range, evidence_strings.len())
                };
                let match_reason = if is_graph {
                    "graph node identifier match"
                } else {
                    "tantivy hybrid lexical match"
                };
                let mut score_breakdown = vec![
                    ScoreComponent::single(
                        "bm25_relevance",
                        score,
                        evidence_ids.clone(),
                        "BM25 score from local Tantivy index",
                    ),
                    ScoreComponent::adjustment(
                        "query_variant_boost",
                        boosted_score - score,
                        evidence_ids.clone(),
                        "query variant, path, symbol, or graph-node boost applied to lexical result",
                    ),
                ];
                if is_graph {
                    score_breakdown.push(ScoreComponent::adjustment(
                        "graph_node_identifier",
                        0.3,
                        evidence_ids.clone(),
                        "indexed graph-node identifiers, qualified names, routes, or properties matched",
                    ));
                }
                results.push(SearchResult {
                    path,
                    line_range,
                    snippet: snippet(&chunk.text, raw_query),
                    symbol,
                    score: boosted_score,
                    match_reason: match_reason.into(),
                    evidence: evidence_strings.clone(),
                    evidence_refs: evidence_ids.clone(),
                    confidence,
                    score_breakdown,
                });
            }
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchDocumentFilter {
    All,
    CodeOnly,
    GraphOnly,
}

pub fn rebuild_disk_index(
    index_dir: impl AsRef<Path>,
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
) -> Result<TantivySearchIndex> {
    let index_dir = index_dir.as_ref();
    if index_dir.exists() {
        fs::remove_dir_all(index_dir)?;
    }
    fs::create_dir_all(index_dir)?;
    let mut index = TantivySearchIndex::open_or_create(index_dir)?;
    index.rebuild(chunks, files, symbols)?;
    Ok(index)
}

pub fn rebuild_disk_index_with_graph(
    index_dir: impl AsRef<Path>,
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    graph_nodes: &[GraphNode],
) -> Result<TantivySearchIndex> {
    let index_dir = index_dir.as_ref();
    if index_dir.exists() {
        fs::remove_dir_all(index_dir)?;
    }
    fs::create_dir_all(index_dir)?;
    let mut index = TantivySearchIndex::open_or_create(index_dir)?;
    index.rebuild_with_graph(chunks, files, symbols, graph_nodes)?;
    Ok(index)
}

pub fn default_index_dir(repo: impl AsRef<Path>) -> PathBuf {
    repo.as_ref().join(".ok/search/tantivy")
}

fn schema() -> Schema {
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("default")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let stored_text = TextOptions::default().set_stored();
    let mut builder = Schema::builder();
    builder.add_text_field("path", text.clone());
    builder.add_text_field("content", text.clone());
    builder.add_text_field("chunk_json", stored_text.clone());
    builder.add_text_field("file_json", stored_text.clone());
    builder.add_text_field("symbol_json", text.clone());
    builder.build()
}

fn query_variants(query: &str) -> Vec<String> {
    let mut variants = vec![query.to_string()];
    let tokens = identifier_tokens(query);
    if tokens.len() > 1 {
        variants.push(tokens.join(" OR "));
        variants.push(tokens.join("_"));
        variants.push(tokens.join("-"));
    }
    if query.contains('_') || query.contains('-') || query.chars().any(char::is_uppercase) {
        let split = split_identifier(query);
        if split.len() > 1 {
            variants.push(split.join(" OR "));
            variants.push(split.join(" "));
        }
    }
    variants.sort();
    variants.dedup();
    variants
}

fn identifier_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .flat_map(split_identifier)
        .collect()
}

fn split_identifier(query: impl AsRef<str>) -> Vec<String> {
    let query = query.as_ref();
    let mut out = Vec::new();
    let mut current = String::new();
    let chars = query.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                out.push(current.to_ascii_lowercase());
                current.clear();
            }
            continue;
        }
        let prev = index.checked_sub(1).and_then(|idx| chars.get(idx)).copied();
        let next = chars.get(index + 1).copied();
        let starts_new_word = ch.is_ascii_uppercase()
            && !current.is_empty()
            && (prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || next.is_some_and(|n| n.is_ascii_lowercase())
                    && prev.is_some_and(|p| p.is_ascii_uppercase()));
        if starts_new_word {
            out.push(current.to_ascii_lowercase());
            current.clear();
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current.to_ascii_lowercase());
    }
    out.into_iter().filter(|token| token.len() >= 2).collect()
}

fn variant_boost(
    query: &str,
    variant: &str,
    file: &File,
    symbol: Option<&Symbol>,
    chunk: &CodeChunk,
) -> f32 {
    let mut boost = 0.0;
    let query_lower = query.to_ascii_lowercase();
    let path = file.path.to_string_lossy().to_ascii_lowercase();
    if path.contains(&query_lower.replace(' ', "_"))
        || path.contains(&query_lower.replace(' ', "-"))
        || path.contains(&query_lower)
    {
        boost += 0.4;
    }
    if let Some(symbol) = symbol {
        let name = symbol.name.to_ascii_lowercase();
        let qualified_name = symbol.qualified_name.to_ascii_lowercase();
        if symbol.id.0.eq_ignore_ascii_case(query) || qualified_name == query_lower {
            boost += 2.0;
        } else if name == query_lower || name.contains(&query_lower.replace(' ', "_")) {
            boost += 1.0;
        }
    }
    let chunk_text = chunk.text.to_ascii_lowercase();
    if is_graph_chunk(chunk) && chunk_text.contains(&query_lower) {
        boost += 0.8;
    }
    if variant != query {
        boost += 0.05;
    }
    boost
}

fn graph_chunk_id(node: &GraphNode) -> String {
    format!("graph-node:{}", node.id.0)
}

fn is_graph_chunk(chunk: &CodeChunk) -> bool {
    chunk.id.starts_with("graph-node:")
}

fn graph_node_text(node: &GraphNode, file: &File, symbol: Option<&Symbol>) -> String {
    let mut parts = vec![
        "graph node".to_string(),
        node.id.0.clone(),
        format!("{:?}", node.node_type),
        node.label.clone(),
        file.path.to_string_lossy().to_string(),
    ];
    if let Some(file_id) = &node.file_id {
        parts.push(file_id.0.clone());
    }
    if let Some(symbol_id) = &node.symbol_id {
        parts.push(symbol_id.0.clone());
    }
    if let Some(symbol) = symbol {
        parts.push(symbol.name.clone());
        parts.push(symbol.qualified_name.clone());
    }
    for value in node.properties.values() {
        if let Some(text) = value.as_str() {
            parts.push(text.to_string());
        } else if value.is_number() || value.is_boolean() {
            parts.push(value.to_string());
        }
    }
    parts.extend(
        parts
            .clone()
            .into_iter()
            .flat_map(|part| identifier_tokens(&part))
            .collect::<Vec<_>>(),
    );
    parts.join("\n")
}

fn fields(schema: Schema) -> Result<TantivyFields> {
    Ok(TantivyFields {
        path: field(&schema, "path")?,
        content: field(&schema, "content")?,
        chunk_json: field(&schema, "chunk_json")?,
        file_json: field(&schema, "file_json")?,
        symbol_json: field(&schema, "symbol_json")?,
    })
}

fn field(schema: &Schema, name: &str) -> Result<Field> {
    schema
        .get_field(name)
        .map_err(|err| OkError::Search(err.to_string()))
}

fn required_json<T: serde::de::DeserializeOwned>(
    document: &TantivyDocument,
    field: Field,
) -> Result<T> {
    let value = document
        .get_first(field)
        .and_then(|value| value.as_str())
        .ok_or_else(|| OkError::Search("tantivy document is missing stored JSON".into()))?;
    serde_json::from_str(value).map_err(Into::into)
}

fn optional_json<T: serde::de::DeserializeOwned>(
    document: &TantivyDocument,
    field: Field,
) -> Result<Option<T>> {
    let Some(value) = document.get_first(field).and_then(|value| value.as_str()) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(value).map(Some).map_err(Into::into)
}

fn snippet(text: &str, query: &str) -> String {
    let normalized_query = query.to_ascii_lowercase();
    text.lines()
        .find(|line| {
            !line.trim().is_empty()
                && line
                    .to_ascii_lowercase()
                    .contains(normalized_query.as_str())
        })
        .or_else(|| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default()
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn search_err(err: tantivy::TantivyError) -> OkError {
    OkError::Search(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        identifier_tokens, rebuild_disk_index, rebuild_disk_index_with_graph, TantivySearchIndex,
    };
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, GraphNode, GraphNodeType,
        Language, LineRange, RepositoryId, Symbol, SymbolId, SymbolKind,
    };
    use open_kioku_storage::SearchIndex;
    use std::collections::BTreeMap;

    #[test]
    fn persists_and_searches_bm25_index() {
        let temp = tempfile::tempdir().unwrap();
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("symbol-1"),
            name: "retry_import".into(),
            qualified_name: "src::lib::retry_import".into(),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 3 },
            language: Language::Rust,
            text: "use std::time::Duration;\npub fn retry_import() {}\n".into(),
            symbol_id: Some(symbol.id.clone()),
        };
        rebuild_disk_index(temp.path(), &[chunk], &[file], &[symbol]).unwrap();
        let index = TantivySearchIndex::open_or_create(temp.path()).unwrap();
        let results = index.search("retry", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(results[0].snippet, "pub fn retry_import() {}");
        assert_eq!(results[0].line_range, Some(LineRange { start: 1, end: 3 }));
        assert_eq!(results[0].match_reason, "tantivy hybrid lexical match");
        assert_eq!(results[0].evidence.len(), 2);
        assert!(results[0].evidence[0].contains("BM25 lexical match"));
        assert_eq!(
            results[0].symbol.as_ref().map(|s| s.name.as_str()),
            Some("retry_import")
        );
    }

    #[test]
    fn natural_language_query_matches_identifier_variant() {
        let temp = tempfile::tempdir().unwrap();
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "src/auth_tokens.rs".into(),
            language: Language::Rust,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 2 },
            language: Language::Rust,
            text: "pub fn issue_token() {}\n".into(),
            symbol_id: None,
        };
        rebuild_disk_index(temp.path(), &[chunk], &[file], &[]).unwrap();
        let index = TantivySearchIndex::open_or_create(temp.path()).unwrap();
        let results = index.search("issue token", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, PathBuf::from("src/auth_tokens.rs"));
    }

    #[test]
    fn identifier_tokenization_handles_common_code_and_route_shapes() {
        assert_eq!(
            identifier_tokens("updateCloudClient"),
            vec!["update", "cloud", "client"]
        );
        assert_eq!(
            identifier_tokens("XMLHttpRequestParser"),
            vec!["xml", "http", "request", "parser"]
        );
        assert_eq!(
            identifier_tokens("rate_limit_handler"),
            vec!["rate", "limit", "handler"]
        );
        assert_eq!(
            identifier_tokens("/api/v1/hotels/{hotelId}/rates"),
            vec!["api", "v1", "hotels", "hotel", "id", "rates"]
        );
    }

    #[test]
    fn graph_node_documents_are_searchable_and_filtered() {
        let temp = tempfile::tempdir().unwrap();
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "src/routes.rs".into(),
            language: Language::Rust,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("symbol-route"),
            name: "publish_invoice_event".into(),
            qualified_name: "billing::routes::publish_invoice_event".into(),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange::single(7)),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 7, end: 9 },
            language: Language::Rust,
            text: "fn publish_invoice_event() {}".into(),
            symbol_id: Some(symbol.id.clone()),
        };
        let graph_node = GraphNode {
            id: open_kioku_core::NodeId::new("symbol:symbol-route"),
            node_type: GraphNodeType::Endpoint,
            label: "POST /api/v1/invoices/{invoiceId}/publish".into(),
            file_id: Some(file.id.clone()),
            symbol_id: Some(symbol.id.clone()),
            properties: BTreeMap::from([
                (
                    "route_path".into(),
                    serde_json::json!("/api/v1/invoices/{invoiceId}/publish"),
                ),
                (
                    "qualified_name".into(),
                    serde_json::json!("billing::routes::publish_invoice_event"),
                ),
            ]),
            ..Default::default()
        };

        rebuild_disk_index_with_graph(
            temp.path(),
            &[chunk],
            &[file],
            &[symbol],
            std::slice::from_ref(&graph_node),
        )
        .unwrap();
        let index = TantivySearchIndex::open_or_create(temp.path()).unwrap();

        let graph_results = index.search_graph("publish invoice event", 10).unwrap();
        assert_eq!(graph_results.len(), 1);
        assert_eq!(graph_results[0].match_reason, "graph node identifier match");
        assert_eq!(graph_results[0].line_range, None);
        assert!(graph_results[0]
            .evidence_refs
            .iter()
            .any(|evidence_ref| evidence_ref == "graph-node:symbol:symbol-route"));
        assert!(graph_results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "graph_node_identifier"));

        let route_results = index.search_graph("room nightly rates", 10).unwrap();
        assert!(route_results.is_empty());
        let route_results = index.search_graph("invoice id publish", 10).unwrap();
        assert_eq!(route_results.len(), 1);
    }

    #[test]
    fn exact_symbol_graph_match_ranks_above_lexical_chunk() {
        let temp = tempfile::tempdir().unwrap();
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "src/billing.rs".into(),
            language: Language::Rust,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("symbol-exact"),
            name: "update_cloud_client".into(),
            qualified_name: "billing::update_cloud_client".into(),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange::single(11)),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 3 },
            language: Language::Rust,
            text: "cloud client update helper mentions billing::update_cloud_client".into(),
            symbol_id: None,
        };
        let graph_node = GraphNode {
            id: open_kioku_core::NodeId::new("symbol:symbol-exact"),
            node_type: GraphNodeType::Function,
            label: "update_cloud_client".into(),
            file_id: Some(file.id.clone()),
            symbol_id: Some(symbol.id.clone()),
            ..Default::default()
        };

        rebuild_disk_index_with_graph(temp.path(), &[chunk], &[file], &[symbol], &[graph_node])
            .unwrap();
        let index = TantivySearchIndex::open_or_create(temp.path()).unwrap();
        let results = index
            .search_all("billing::update_cloud_client", 10)
            .unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].match_reason, "graph node identifier match");
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "graph_node_identifier"));
    }

    use std::path::PathBuf;
}
