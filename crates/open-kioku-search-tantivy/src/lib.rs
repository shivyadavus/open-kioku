use open_kioku_core::{CodeChunk, File, SearchResult, Symbol};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::SearchIndex;
use serde_json;
use std::path::{Path, PathBuf};
use tantivy::{
    collector::TopDocs,
    directory::MmapDirectory,
    doc,
    query::QueryParser,
    schema::{SchemaBuilder, FAST, STORED, TEXT},
    Index, IndexWriter, TantivyDocument,
};

pub struct TantivySearchIndex {
    index: Index,
    writer: Option<IndexWriter>,
}

fn search_err(err: tantivy::TantivyError) -> OkError {
    OkError::Search(err.to_string())
}

impl TantivySearchIndex {
    pub fn open(path: &Path) -> Result<Self> {
        let mut schema_builder = SchemaBuilder::new();
        schema_builder.add_text_field("id", STORED);
        schema_builder.add_text_field("text", TEXT | STORED);
        schema_builder.add_text_field("path", STORED | FAST);
        schema_builder.add_text_field("payload", STORED);
        let schema = schema_builder.build();
        std::fs::create_dir_all(path).map_err(|err| OkError::Storage(err.to_string()))?;
        let dir = MmapDirectory::open(path).map_err(search_err)?;
        let index = Index::open_or_create(dir, schema).map_err(search_err)?;
        Ok(Self { index, writer: None })
    }
}

pub fn default_index_dir(repo: &Path) -> PathBuf {
    repo.join(".ok/tantivy")
}

impl SearchIndex for TantivySearchIndex {
    fn rebuild(
        &mut self,
        chunks: &[CodeChunk],
        files: &[File],
        _symbols: &[Symbol],
    ) -> Result<()> {
        let schema = self.index.schema();
        let id_field = schema.get_field("id").map_err(search_err)?;
        let text_field = schema.get_field("text").map_err(search_err)?;
        let path_field = schema.get_field("path").map_err(search_err)?;
        let payload_field = schema.get_field("payload").map_err(search_err)?;
        let mut writer: IndexWriter = self
            .index
            .writer(50_000_000)
            .map_err(search_err)?;
        writer.delete_all_documents().map_err(search_err)?;
        for chunk in chunks {
            let file_path = files
                .iter()
                .find(|f| f.id == chunk.file_id)
                .map(|f| f.path.display().to_string())
                .unwrap_or_default();
            let payload = serde_json::to_string(chunk)
                .map_err(|err| OkError::Storage(err.to_string()))?;
            writer
                .add_document(doc!(
                    id_field => chunk.id.clone(),
                    text_field => chunk.text.clone(),
                    path_field => file_path,
                    payload_field => payload,
                ))
                .map_err(search_err)?;
        }
        writer.commit().map_err(search_err)?;
        self.writer = Some(writer);
        Ok(())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let schema = self.index.schema();
        let text_field = schema.get_field("text").map_err(search_err)?;
        let path_field = schema.get_field("path").map_err(search_err)?;
        let payload_field = schema.get_field("payload").map_err(search_err)?;
        let reader = self
            .index
            .reader()
            .map_err(search_err)?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![text_field]);
        let query = parser
            .parse_query(query)
            .map_err(|err| OkError::Search(err.to_string()))?;
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit))
            .map_err(search_err)?;
        let mut results = Vec::new();
        for (score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr).map_err(search_err)?;
            let path_val = doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let payload_val = doc
                .get_first(payload_field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| OkError::Search("tantivy document is missing stored JSON".into()))?;
            let chunk: CodeChunk = serde_json::from_str(payload_val)
                .map_err(|err| OkError::Storage(err.to_string()))?;
            results.push(SearchResult {
                path: PathBuf::from(path_val),
                score: score as f32,
                snippet: chunk.text.chars().take(200).collect(),
                symbol: None,
                evidence: Vec::new(),
            });
        }
        Ok(results)
    }
}
