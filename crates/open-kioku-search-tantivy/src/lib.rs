use open_kioku_core::{CodeChunk, File, SearchResult, Symbol};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::EvidenceBuilder;
use open_kioku_storage::SearchIndex;
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
        for chunk in chunks {
            let Some(file) = files.iter().find(|file| file.id == chunk.file_id) else {
                continue;
            };
            let symbol = chunk
                .symbol_id
                .as_ref()
                .and_then(|id| symbols.iter().find(|symbol| symbol.id == *id));
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
        let raw_query = query;
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
        let query = parser
            .parse_query(query)
            .map_err(|err| OkError::Search(err.to_string()))?;
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit))
            .map_err(search_err)?;
        let mut results = Vec::with_capacity(top_docs.len());
        for (score, address) in top_docs {
            let document: TantivyDocument = searcher.doc(address).map_err(search_err)?;
            let chunk: CodeChunk = required_json(&document, self.fields.chunk_json)?;
            let file: File = required_json(&document, self.fields.file_json)?;
            let symbol: Option<Symbol> = optional_json(&document, self.fields.symbol_json)?;
            let (evidence_strings, confidence) = EvidenceBuilder::new()
                .add("BM25 lexical match from local Tantivy index", score)
                .build();
            results.push(SearchResult {
                path: file.path,
                line_range: Some(chunk.range),
                snippet: snippet(&chunk.text, raw_query),
                symbol,
                score,
                match_reason: "tantivy bm25 lexical match".into(),
                evidence: evidence_strings,
                confidence,
            });
        }
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
        assert_eq!(results[0].match_reason, "tantivy bm25 lexical match");
        assert_eq!(results[0].evidence.len(), 1);
        assert!(results[0].evidence[0].contains("BM25 lexical match"));
        assert_eq!(
            results[0].symbol.as_ref().map(|s| s.name.as_str()),
            Some("retry_import")
        );
    }

    use std::path::PathBuf;
}
