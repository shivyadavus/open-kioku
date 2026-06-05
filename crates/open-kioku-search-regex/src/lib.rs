use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, LineRange, ScoreComponent, SearchResult, Symbol,
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
        let tokens = query_tokens(query);
        let re =
            Regex::new(&regex::escape(query)).map_err(|err| OkError::Search(err.to_string()))?;
        let mut results = Vec::new();
        for chunk in &self.chunks {
            let Some(file) = self.files.get(&chunk.file_id.0) else {
                continue;
            };
            let haystack = format!("{} {}", file.path.display(), chunk.text);
            let lower = haystack.to_ascii_lowercase();
            let normalized = normalize_for_search(&haystack);
            let exact_match = re.is_match(&lower) || lower.contains(&query.to_ascii_lowercase());
            let token_match =
                !tokens.is_empty() && tokens.iter().all(|token| normalized.contains(token));
            if !exact_match && !token_match {
                continue;
            }
            let snippet = best_snippet(&chunk.text, query, &tokens);
            let score = lexical_score(&haystack, query, &tokens, file.is_generated, file.is_vendor);
            let (evidence_strings, confidence) = EvidenceBuilder::new()
                .add(format!("lexical match for `{query}`"), score)
                .build();
            let line_range = Some(chunk.range.clone());
            let evidence_ids =
                search_result_evidence_ids(&file.path, &line_range, evidence_strings.len());
            results.push(SearchResult {
                path: file.path.clone(),
                line_range,
                snippet,
                symbol: chunk
                    .symbol_id
                    .as_ref()
                    .and_then(|id| self.symbols_by_chunk.get(&id.0).cloned()),
                score,
                match_reason: "lexical substring match".into(),
                evidence: evidence_strings.clone(),
                confidence,
                score_breakdown: vec![ScoreComponent::single(
                    "lexical_relevance",
                    score,
                    evidence_ids,
                    "lexical phrase/token score adjusted for generated and vendor paths",
                )],
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
            let line_range = Some(LineRange::single(line_number));
            let evidence_ids =
                search_result_evidence_ids(&path, &line_range, evidence_strings.len());
            results.push(SearchResult {
                path: path.clone(),
                line_range,
                snippet: line.trim().to_string(),
                symbol: None,
                score: 1.0,
                match_reason: "regex match".into(),
                evidence: evidence_strings.clone(),
                confidence,
                score_breakdown: vec![ScoreComponent::single(
                    "regex_match",
                    1.0,
                    evidence_ids,
                    "direct regex line match",
                )],
            });
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}

fn query_tokens(query: &str) -> Vec<String> {
    normalize_for_search(query)
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_for_search(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn best_snippet(text: &str, query: &str, tokens: &[String]) -> String {
    let lower = query.to_ascii_lowercase();
    text.lines()
        .find(|line| line.to_ascii_lowercase().contains(&lower))
        .or_else(|| {
            text.lines().max_by_key(|line| {
                let normalized = normalize_for_search(line);
                tokens
                    .iter()
                    .filter(|token| normalized.contains(token.as_str()))
                    .count()
            })
        })
        .unwrap_or_else(|| text.lines().next().unwrap_or_default())
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn lexical_score(text: &str, query: &str, tokens: &[String], generated: bool, vendor: bool) -> f32 {
    let lower = text.to_ascii_lowercase();
    let normalized = normalize_for_search(text);
    let q = query.to_ascii_lowercase();
    let phrase_hits = lower.matches(&q).count() as f32;
    let token_hits = tokens
        .iter()
        .filter(|token| normalized.contains(token.as_str()))
        .count() as f32;
    let mut score = 0.35 + phrase_hits.min(5.0) * 0.12 + token_hits.min(5.0) * 0.08;
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
    fn lexical_search_matches_multi_word_query_against_snake_case() {
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "src/mcp.rs".into(),
            language: Language::Rust,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 10, end: 12 },
            language: Language::Rust,
            text: "pub fn search_code(query: &str) {}\npub fn repo_status() {}\n".into(),
            symbol_id: None,
        };

        let results = search_chunks(&[chunk], &[file], &[], "search code", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "pub fn search_code(query: &str) {}");
    }

    #[test]
    fn lexical_search_matches_query_against_file_path() {
        let file = File {
            id: FileId::new("file-1"),
            repository_id: RepositoryId::new("repo-1"),
            path: "packages/npm/package.json".into(),
            language: Language::Json,
            size_bytes: 42,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let chunk = CodeChunk {
            id: "chunk-1".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 3 },
            language: Language::Json,
            text: r#"{ "name": "open-kioku" }"#.into(),
            symbol_id: None,
        };

        let results = search_chunks(&[chunk], &[file], &[], "npm package", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, PathBuf::from("packages/npm/package.json"));
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
