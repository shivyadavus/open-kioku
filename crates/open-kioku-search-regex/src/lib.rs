use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, LineRange, ScoreComponent, SearchResult, Symbol,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::EvidenceBuilder;
use open_kioku_storage::{MetadataStore, SearchIndex};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Upper bound on the files one indexed regex scan will visit. The walk is
/// already paged so only a single file's chunks are resident at a time; this
/// caps the walk itself, so a cheap pattern over a very large repository still
/// terminates and says that it stopped early.
pub const MAX_REGEX_SCAN_FILES: usize = 20_000;

/// Files fetched per `list_files` page during an indexed regex scan.
const REGEX_SCAN_FILE_PAGE: usize = 512;

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
                evidence_refs: evidence_ids.clone(),
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

/// Result of evaluating a pattern over the indexed corpus.
///
/// `files_capped` is the honest half of this type: the walk is bounded, so a
/// caller has to be able to tell "no more matches exist" apart from "we stopped
/// looking".
#[derive(Debug, Default)]
pub struct RegexScan {
    pub results: Vec<SearchResult>,
    pub files_scanned: usize,
    pub files_capped: bool,
}

/// Evaluates `pattern` line by line over indexed chunk text, file by file, in
/// `list_files` order (path-ordered in SQLite), and stops at `limit` matches.
///
/// Only indexed chunk text is searched. Regions the indexer never chunked — the
/// preamble above the first symbol of a symbol-chunked file, unparsed or skipped
/// files — are not in the corpus and therefore not in the answer.
pub fn regex_search_index(
    store: &dyn MetadataStore,
    pattern: &str,
    limit: usize,
) -> Result<RegexScan> {
    let regex = compile(pattern)?;
    let mut scan = RegexScan::default();
    if limit == 0 {
        return Ok(scan);
    }
    let mut offset = 0usize;
    loop {
        let files = store.list_files(REGEX_SCAN_FILE_PAGE, offset)?;
        if files.is_empty() {
            return Ok(scan);
        }
        let page = files.len();
        for file in files {
            if scan.files_scanned >= MAX_REGEX_SCAN_FILES {
                scan.files_capped = true;
                return Ok(scan);
            }
            scan.files_scanned += 1;
            let mut chunks = store.chunks_for_file(&file.id)?;
            chunks.sort_by_key(|chunk| chunk.range.start);
            for chunk in chunks {
                let window = MatchWindow {
                    path: &file.path,
                    first_line: chunk.range.start,
                    last_line: Some(chunk.range.end),
                };
                push_regex_matches(
                    &regex,
                    pattern,
                    window,
                    &chunk.text,
                    limit,
                    &mut scan.results,
                );
                if scan.results.len() >= limit {
                    return Ok(scan);
                }
            }
        }
        if page < REGEX_SCAN_FILE_PAGE {
            return Ok(scan);
        }
        offset = offset.saturating_add(page);
    }
}

pub fn regex_search_file(
    path: PathBuf,
    content: &str,
    pattern: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let regex = compile(pattern)?;
    let mut results = Vec::new();
    let window = MatchWindow {
        path: &path,
        first_line: 1,
        last_line: None,
    };
    push_regex_matches(&regex, pattern, window, content, limit, &mut results);
    Ok(results)
}

fn compile(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|err| OkError::Search(err.to_string()))
}

/// Where a block of text sits in the file it came from, so a match is reported
/// at the file's own line number rather than at an offset into the block.
struct MatchWindow<'a> {
    path: &'a Path,
    first_line: u32,
    /// Last line the block's owner declares it covers, when it declares one.
    last_line: Option<u32>,
}

/// Evidence ids are derived from the final, file-absolute range.
fn push_regex_matches(
    regex: &Regex,
    pattern: &str,
    window: MatchWindow<'_>,
    content: &str,
    limit: usize,
    results: &mut Vec<SearchResult>,
) {
    let MatchWindow {
        path,
        first_line,
        last_line,
    } = window;
    for (idx, line) in content.lines().enumerate() {
        if results.len() >= limit {
            return;
        }
        let line_number = first_line.saturating_add(idx as u32);
        // A chunk's stored text can outrun its declared range; trust the range,
        // so no match is ever reported at a line the chunk does not own.
        if last_line.is_some_and(|last| line_number > last) {
            return;
        }
        if !regex.is_match(line) {
            continue;
        }
        let (evidence_strings, confidence) = EvidenceBuilder::new()
            .add(format!("regex match for `{pattern}`"), 1.0)
            .build();
        let line_range = Some(LineRange::single(line_number));
        let evidence_ids = search_result_evidence_ids(path, &line_range, evidence_strings.len());
        results.push(SearchResult {
            path: path.to_path_buf(),
            line_range,
            snippet: line.trim().to_string(),
            symbol: None,
            score: 1.0,
            match_reason: "regex match".into(),
            evidence: evidence_strings.clone(),
            evidence_refs: evidence_ids.clone(),
            confidence,
            score_breakdown: vec![ScoreComponent::single(
                "regex_match",
                1.0,
                evidence_ids,
                "direct regex line match",
            )],
        });
    }
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
    use super::{regex_search_file, regex_search_index, search_chunks};
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, Import, IndexManifest, Language,
        LineRange, RepositoryId, Symbol, SymbolId, SymbolKind, SymbolOccurrence, TestTarget,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{IndexData, MetadataStore};
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct MemoryStore {
        files: Vec<File>,
        chunks: Vec<CodeChunk>,
    }

    impl MetadataStore for MemoryStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, limit: usize, offset: usize) -> Result<Vec<File>> {
            let mut files = self.files.clone();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(files.into_iter().skip(offset).take(limit).collect())
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok(self.files.iter().find(|file| file.path == path).cloned())
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(self
                .chunks
                .iter()
                .filter(|chunk| chunk.file_id == *file_id)
                .cloned()
                .collect())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(self.chunks.clone())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(Vec::new())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn references_for_symbol(
            &self,
            _id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }

        fn occurrences_for_file(&self, _file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }
    }

    fn indexed_file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo-1"),
            path: path.into(),
            language: Language::Rust,
            size_bytes: 64,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn indexed_chunk(id: &str, file_id: &str, start: u32, text: &str) -> CodeChunk {
        let end = start + text.lines().count().saturating_sub(1) as u32;
        CodeChunk {
            id: id.into(),
            file_id: FileId::new(file_id),
            range: LineRange { start, end },
            language: Language::Rust,
            text: text.into(),
            symbol_id: None,
        }
    }

    fn two_file_store() -> MemoryStore {
        MemoryStore {
            files: vec![
                indexed_file("file-a", "src/a.rs"),
                indexed_file("file-b", "src/b.rs"),
            ],
            chunks: vec![
                indexed_chunk(
                    "chunk-a",
                    "file-a",
                    7,
                    "pub fn retry_import() {}\nfn helper() {}\npub fn retry_export() {}",
                ),
                indexed_chunk("chunk-b", "file-b", 1, "pub fn retry_publish() {}"),
            ],
        }
    }

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
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
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

    #[test]
    fn indexed_regex_scan_reports_absolute_line_numbers_across_files() {
        let store = two_file_store();

        let scan = regex_search_index(&store, r"^pub fn retry_", 20).unwrap();

        assert_eq!(scan.files_scanned, 2);
        assert!(!scan.files_capped);
        let located = scan
            .results
            .iter()
            .map(|result| {
                (
                    result.path.to_string_lossy().to_string(),
                    result.line_range.as_ref().map(|range| range.start),
                )
            })
            .collect::<Vec<_>>();
        // Chunk `chunk-a` starts at line 7, so its second and third lines are
        // file lines 7 and 9 rather than window offsets 1 and 3.
        assert_eq!(
            located,
            vec![
                ("src/a.rs".to_string(), Some(7)),
                ("src/a.rs".to_string(), Some(9)),
                ("src/b.rs".to_string(), Some(1)),
            ]
        );
        assert!(scan
            .results
            .iter()
            .all(|result| result.match_reason == "regex match" && result.confidence >= 1.0));
        assert!(scan
            .results
            .iter()
            .all(|result| !result.evidence_refs.is_empty()));
    }

    #[test]
    fn indexed_regex_scan_returns_no_results_when_nothing_matches() {
        let scan = regex_search_index(&two_file_store(), r"^struct\s+Ledger", 20).unwrap();

        assert!(scan.results.is_empty());
        assert_eq!(scan.files_scanned, 2);
        assert!(!scan.files_capped);
    }

    #[test]
    fn indexed_regex_scan_rejects_an_invalid_pattern() {
        let error = regex_search_index(&two_file_store(), "fn (", 20).unwrap_err();

        assert!(
            error.to_string().contains("search error"),
            "expected a search error, got: {error}"
        );
    }

    #[test]
    fn indexed_regex_scan_stops_at_the_requested_limit() {
        let scan = regex_search_index(&two_file_store(), r"fn ", 2).unwrap();

        assert_eq!(scan.results.len(), 2);
        // The walk stops inside the first file, so the second is never opened.
        assert_eq!(scan.files_scanned, 1);
    }
}
