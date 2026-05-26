use open_kioku_core::{
    CodeChunk, File, FileId, Language, LineRange, Repository, RepositoryId, Symbol, SymbolId,
    SymbolKind,
};
use open_kioku_errors::{OkError, Result};
use protobuf::Message;
use scip::types::Index;
use std::path::{Path, PathBuf};

pub fn parse_scip_index(path: &Path) -> Result<ScipIndex> {
    let bytes = std::fs::read(path).map_err(|err| OkError::Index(err.to_string()))?;
    let index = Index::parse_from_bytes(&bytes).map_err(|err| OkError::Index(err.to_string()))?;
    let repo_id = RepositoryId::new("scip");
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut chunks = Vec::new();
    for document in &index.documents {
        let path = PathBuf::from(&document.relative_path);
        let file_id = FileId::new(document.relative_path.clone());
        files.push(File {
            id: file_id.clone(),
            repository_id: repo_id.clone(),
            path: path.clone(),
            language: Language::Other,
            size_bytes: 0,
            content_hash: String::new(),
            is_generated: false,
            is_vendor: false,
        });
        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() {
                continue;
            }
            let symbol_id = SymbolId::new(occurrence.symbol.clone());
            let range = if occurrence.range.len() >= 2 {
                LineRange::single(occurrence.range[0] as u32 + 1)
            } else {
                LineRange::single(1)
            };
            chunks.push(CodeChunk {
                id: format!("scip:{}:{}", document.relative_path, occurrence.symbol),
                file_id: file_id.clone(),
                symbol_id: Some(symbol_id.clone()),
                language: Language::Other,
                text: occurrence.symbol.clone(),
                range,
            });
            symbols.push(Symbol {
                id: symbol_id,
                name: occurrence.symbol.split('/').last().unwrap_or(&occurrence.symbol).into(),
                kind: SymbolKind::Function,
                file_id: file_id.clone(),
                range: LineRange::single(occurrence.range.first().copied().unwrap_or(0) as u32 + 1),
                signature: None,
                doc_comment: None,
            });
        }
    }
    symbols.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    symbols.dedup_by(|a, b| a.id == b.id);
    Ok(ScipIndex { files, symbols, chunks })
}

pub struct ScipIndex {
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
}
