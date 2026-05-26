use open_kioku_core::{CodeChunk, File, SearchResult, Symbol};
use open_kioku_errors::{OkError, Result};
use regex::Regex;
use std::path::PathBuf;

pub fn search_chunks(
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let pattern = build_pattern(query)?;
    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in chunks {
        if !pattern.is_match(&chunk.text) {
            continue;
        }
        if !seen.insert(chunk.file_id.clone()) {
            continue;
        }
        let file = files.iter().find(|f| f.id == chunk.file_id);
        let symbol = symbols
            .iter()
            .find(|s| chunk.symbol_id.as_ref() == Some(&s.id))
            .cloned();
        let path: PathBuf = file
            .map(|f| f.path.clone())
            .unwrap_or_else(|| PathBuf::from(chunk.file_id.0.clone()));
        let score = score_match(&chunk.text, query);
        results.push(SearchResult {
            path,
            score,
            snippet: chunk.text.chars().take(200).collect(),
            symbol,
            evidence: Vec::new(),
        });
        if results.len() >= limit {
            break;
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(results)
}

fn build_pattern(query: &str) -> Result<Regex> {
    let escaped = regex::escape(query);
    // Try exact match first, fall back to word-boundary match
    Regex::new(&format!(r"(?i){}", escaped)).map_err(|err| OkError::Search(err.to_string()))
}

pub fn regex_search(
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    pattern: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let regex = Regex::new(pattern).map_err(|err| OkError::Search(err.to_string()))?;
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in chunks {
        if !regex.is_match(&chunk.text) {
            continue;
        }
        if !seen.insert(chunk.file_id.clone()) {
            continue;
        }
        let file = files.iter().find(|f| f.id == chunk.file_id);
        let symbol = symbols
            .iter()
            .find(|s| chunk.symbol_id.as_ref() == Some(&s.id))
            .cloned();
        let path: PathBuf = file
            .map(|f| f.path.clone())
            .unwrap_or_else(|| PathBuf::from(chunk.file_id.0.clone()));
        results.push(SearchResult {
            path,
            score: 0.5,
            snippet: chunk.text.chars().take(200).collect(),
            symbol,
            evidence: Vec::new(),
        });
        if results.len() >= limit {
            break;
        }
    }
    Ok(results)
}

fn score_match(text: &str, query: &str) -> f32 {
    let lower_text = text.to_ascii_lowercase();
    let lower_query = query.to_ascii_lowercase();
    let terms: Vec<&str> = lower_query.split_whitespace().collect();
    if terms.is_empty() {
        return 0.0;
    }
    let matched = terms.iter().filter(|&&t| lower_text.contains(t)).count();
    let base = matched as f32 / terms.len() as f32;
    // Bonus for exact phrase match
    if lower_text.contains(&lower_query) {
        base + 0.3
    } else {
        base
    }
}
