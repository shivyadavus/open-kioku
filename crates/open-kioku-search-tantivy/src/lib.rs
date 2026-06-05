use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, ScoreComponent, SearchResult, Symbol,
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
        writer.commit().map_err(search_err)?;
        Ok(())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
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
                    &TopDocs::with_limit(limit.saturating_mul(2).max(limit)),
                )
                .map_err(search_err)?;
            for (score, address) in top_docs {
                let document: TantivyDocument = searcher.doc(address).map_err(search_err)?;
                let chunk: CodeChunk = required_json(&document, self.fields.chunk_json)?;
                let file: File = required_json(&document, self.fields.file_json)?;
                let key = format!("{}:{}:{}", file.path.display(), chunk.range.start, chunk.id);
                if !seen.insert(key) {
                    continue;
                }
                let symbol: Option<Symbol> = optional_json(&document, self.fields.symbol_json)?;
                let boosted_score =
                    score + variant_boost(raw_query, &variant, &file, symbol.as_ref());
                let (evidence_strings, confidence) = EvidenceBuilder::new()
                    .add("BM25 lexical match from local Tantivy index", score)
                    .add(
                        format!("query variant `{variant}` matched local index"),
                        boosted_score,
                    )
                    .build();
                let path = file.path;
                let line_range = Some(chunk.range.clone());
                let evidence_ids =
                    search_result_evidence_ids(&path, &line_range, evidence_strings.len());
                results.push(SearchResult {
                    path,
                    line_range,
                    snippet: snippet(&chunk.text, raw_query),
                    symbol,
                    score: boosted_score,
                    match_reason: "tantivy hybrid lexical match".into(),
                    evidence: evidence_strings.clone(),
                    evidence_refs: evidence_ids.clone(),
                    confidence,
                    score_breakdown: vec![
                        ScoreComponent::single(
                            "bm25_relevance",
                            score,
                            evidence_ids.clone(),
                            "BM25 score from local Tantivy index",
                        ),
                        ScoreComponent::adjustment(
                            "query_variant_boost",
                            boosted_score - score,
                            evidence_ids,
                            "query variant, path, or symbol boost applied to lexical result",
                        ),
                    ],
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
    let tokens = query_tokens(query);
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

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn split_identifier(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in query.chars() {
        if ch == '_' || ch == '-' || ch == '/' || ch == '.' {
            if !current.is_empty() {
                out.push(current.to_ascii_lowercase());
                current.clear();
            }
            previous_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower && !current.is_empty() {
            out.push(current.to_ascii_lowercase());
            current.clear();
        }
        previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current.to_ascii_lowercase());
    }
    out.into_iter().filter(|token| token.len() >= 2).collect()
}

fn variant_boost(query: &str, variant: &str, file: &File, symbol: Option<&Symbol>) -> f32 {
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
        if name == query_lower || name.contains(&query_lower.replace(' ', "_")) {
            boost += 0.5;
        }
    }
    if variant != query {
        boost += 0.05;
    }
    boost
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
    use super::{rebuild_disk_index, TantivySearchIndex};
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, Language, LineRange, RepositoryId,
        Symbol, SymbolId, SymbolKind,
    };
    use open_kioku_storage::SearchIndex;

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

    use std::path::PathBuf;
}
