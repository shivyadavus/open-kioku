use open_kioku_core::{
    CodeChunk, File, LineRange, SearchResult, Symbol,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::EvidenceBuilder;
use open_kioku_storage::SearchIndex;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub struct MemorySearchIndex {
    files: HashMap<String, File>,
    symbols_by_chunk: HashMap<String, Symbol>,
    chunks: Vec<CodeChunk>,
}

impl MemorySearchIndex {
    pub fn from_parts(chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) -> Self {
        let mut index = Self::default();
        index.replace(chunks, files, symbols);
        index
    }
}

impl SearchIndex for MemorySearchIndex {
    fn rebuild(&mut self, chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) -> Result<()> {
        self.replace(chunks, files, symbols);
        Ok(())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let re =
            Regex::new(&regex::escape(query)).map_err(|err| OkError::Search(err.to_string()))?;
        let mut results = Vec::new();
        for chunk in &self.chunks {
            if !re.is_match(&chunk.text.to_ascii_lowercase())
                && !chunk
                    .text
                    .to_ascii_lowercase()
                    .contains(&query.to_ascii_lowercase())
            {
                continue;
            }
            let Some(file) = self.files.get(&chunk.file_id.0) else {
                continue;
            };
            let snippet = best_snippet(&chunk.text, query);
            let score = lexical_score(&chunk.text, query, file.is_generated, file.is_vendor);
            let (evidence_strings, confidence) = EvidenceBuilder::new()
                .add(format!("lexical match for `{query}`"), score)
                .build();
            results.push(SearchResult {
                path: file.path.clone(),
                line_range: Some(chunk.range.clone()),
                snippet,
                symbol: chunk
                    .symbol_id
                    .as_ref()
                    .and_then(|id| self.symbols_by_chunk.get(&id.0).cloned()),
                score,
                match_reason: "lexical substring match".into(),
                evidence: evidence_strings,
                confidence,
            });
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

impl MemorySearchIndex {
    pub fn replace(&mut self, chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) {
        self.files = files
            .iter()
            .map(|file| (file.id.0.clone(), file.clone()))
            .collect();
        self.symbols_by_chunk = symbols
            .iter()
            .map(|symbol| (symbol.id.0.clone(), symbol.clone()))
            .collect();
        self.chunks = chunks.to_vec();
    }
}

impl From<(&[CodeChunk], &[File], &[Symbol])> for MemorySearchIndex {
    fn from(value: (&[CodeChunk], &[File], &[Symbol])) -> Self {
        let mut index = Self::default();
        index.replace(value.0, value.1, value.2);
        index
    }
}

pub fn search_chunks(
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let index = MemorySearchIndex::from((chunks, files, symbols));
    index.search(query, limit)
}

pub fn regex_search_file(
    path: PathBuf,
    content: &str,
    pattern: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let regex = Regex::new(pattern).map_err(|err| OkError::Search(err.to_string()))?;
    let mut results = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            let line_number = (idx + 1) as u32;
            let (evidence_strings, confidence) = EvidenceBuilder::new()
                .add(format!("regex match for `{pattern}`"), 1.0)
                .build();
            results.push(SearchResult {
                path: path.clone(),
                line_range: Some(LineRange::single(line_number)),
                snippet: line.trim().to_string(),
                symbol: None,
                score: 1.0,
                match_reason: "regex match".into(),
                evidence: evidence_strings,
                confidence,
            });
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}

fn best_snippet(text: &str, query: &str) -> String {
    let lower = query.to_ascii_lowercase();
    text.lines()
        .find(|line| line.to_ascii_lowercase().contains(&lower))
        .unwrap_or_else(|| text.lines().next().unwrap_or_default())
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn lexical_score(text: &str, query: &str, generated: bool, vendor: bool) -> f32 {
    let lower = text.to_ascii_lowercase();
    let q = query.to_ascii_lowercase();
    let hits = lower.matches(&q).count() as f32;
    let mut score = 0.4 + hits.min(5.0) * 0.12;
    if generated {
        score *= 0.55;
    }
    if vendor {
        score *= 0.35;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::{regex_search_file, search_chunks};
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, Language, LineRange, RepositoryId,
        Symbol, SymbolId, SymbolKind,
    };
    use std::path::PathBuf;

    #[test]
    fn lexical_search_returns_query_line_and_evidence() {
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
            range: Some(LineRange::single(2)),
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

        let results = search_chunks(&[chunk], &[file], &[symbol], "retry", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "pub fn retry_import() {}");
        assert_eq!(results[0].line_range, Some(LineRange { start: 1, end: 3 }));
        assert_eq!(results[0].match_reason, "lexical substring match");
        assert_eq!(results[0].evidence.len(), 1);
        assert!(results[0].evidence[0].contains("lexical match"));
        assert_eq!(
            results[0].symbol.as_ref().map(|s| s.name.as_str()),
            Some("retry_import")
        );
    }

    #[test]
    fn regex_file_search_returns_line_level_evidence() {
        let results = regex_search_file(
            PathBuf::from("src/lib.rs"),
            "fn first() {}\nfn retry_import() {}\n",
            "retry_.*",
            10,
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_range, Some(LineRange::single(2)));
        assert_eq!(results[0].snippet, "fn retry_import() {}");
        assert_eq!(results[0].evidence.len(), 1);
        assert!(results[0].evidence[0].contains("regex match"));
    }
}
