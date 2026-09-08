use open_kioku_core::{
    CodeChunk, Confidence, EvidenceSourceType, LineRange, Symbol, SymbolContext, SymbolId,
    SymbolKind, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::MetadataStore;

/// Lines of already-indexed context returned on each side of a definition.
pub const SYMBOL_CONTEXT_SURROUNDING_LINES: u32 = 10;

/// Cap on the definition lines one context bundle returns. A definition longer
/// than this is cut and marked `truncated` rather than silently trimmed.
pub const SYMBOL_CONTEXT_MAX_BODY_LINES: u32 = 400;

pub struct SymbolEngine<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> SymbolEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        self.store.list_symbols(Some(query), limit, 0)
    }

    pub fn definition(&self, query: &str) -> Result<Symbol> {
        // Indexed exact-name fast path first; the substring scan only runs when the query is a
        // qualified-name fragment (or otherwise not an exact identity).
        let mut matches = self
            .store
            .symbols_named(query, 250)?
            .into_iter()
            .filter(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            matches = self
                .find(query, 250)?
                .into_iter()
                .filter(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
                .collect::<Vec<_>>();
        }
        matches.sort_by_key(|symbol| definition_rank(symbol, query));
        matches
            .into_iter()
            .next()
            .ok_or_else(|| OkError::SymbolNotFound(query.into()))
    }

    pub fn by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
        self.store.symbol_by_id(id)
    }

    /// Joins a definition back to the indexed chunk text that covers it, so a
    /// caller gets the body and its surrounding lines rather than just the
    /// symbol record.
    ///
    /// The corpus is the index, not the working tree. Whatever cannot be
    /// recovered from it comes back as a caveat rather than as a shorter answer
    /// that reads like a complete one.
    pub fn context(&self, query: &str, surrounding_lines: u32) -> Result<SymbolContext> {
        let symbol = self.definition(query)?;
        let mut context = SymbolContext {
            symbol,
            path: None,
            body: None,
            body_range: None,
            leading_lines: Vec::new(),
            leading_range: None,
            trailing_lines: Vec::new(),
            trailing_range: None,
            truncated: false,
            evidence: Vec::new(),
            caveats: Vec::new(),
        };

        match self.store.file_by_id(&context.symbol.file_id)? {
            Some(file) => context.path = Some(file.path),
            None => {
                let caveat = format!(
                    "indexed symbol `{}` references file id `{}`, which is no longer indexed; its path and body could not be resolved",
                    context.symbol.qualified_name, context.symbol.file_id.0
                );
                context.caveats.push(caveat);
            }
        }
        let path_label = display_path(&context);

        let Some(symbol_range) = context.symbol.range.clone() else {
            context.caveats.push(format!(
                "indexed symbol `{}` carries no line range, so its definition body could not be located",
                context.symbol.qualified_name
            ));
            return Ok(context);
        };

        let mut chunks = self.store.chunks_for_file(&context.symbol.file_id)?;
        chunks.sort_by_key(|chunk| chunk.range.start);
        let Some(covering) = chunks.iter().find(|chunk| {
            chunk.range.start <= symbol_range.start && symbol_range.start <= chunk.range.end
        }) else {
            context.caveats.push(format!(
                "no indexed chunk covers {} line {}, so the definition body could not be recovered; re-index the repository to restore it",
                path_label,
                symbol_range.start
            ));
            return Ok(context);
        };

        // A symbol whose recorded range is a single line does not tell us where
        // its body ends, so the covering chunk's extent stands in. Chunks are
        // cut at the next symbol's first line, which usually overshoots by the
        // following definition's doc comment — say so rather than presenting a
        // chunk boundary as a parsed end of definition.
        let derived_end =
            symbol_range.end <= symbol_range.start && covering.range.end > symbol_range.start;
        let mut body_end = if derived_end {
            covering.range.end
        } else {
            symbol_range.end.max(symbol_range.start)
        };
        let max_end = symbol_range
            .start
            .saturating_add(SYMBOL_CONTEXT_MAX_BODY_LINES.saturating_sub(1));
        if body_end > max_end {
            body_end = max_end;
            context.truncated = true;
            context.caveats.push(format!(
                "the definition body was cut at {SYMBOL_CONTEXT_MAX_BODY_LINES} lines; the remainder is not included"
            ));
        }

        let body = indexed_lines(&chunks, symbol_range.start, body_end);
        if body.is_empty() {
            context.caveats.push(format!(
                "the chunk covering {} line {} holds no text for the definition",
                path_label, symbol_range.start
            ));
            return Ok(context);
        }
        let body_range = LineRange {
            start: body
                .first()
                .map(|(line, _)| *line)
                .unwrap_or(symbol_range.start),
            end: body.last().map(|(line, _)| *line).unwrap_or(body_end),
        };
        context.evidence.push(format!(
            "definition body recovered from indexed chunk `{}` covering {} lines {}-{}",
            covering.id, path_label, covering.range.start, covering.range.end
        ));
        // A chunk's declared range can outrun the text it stores, and lines can
        // fall between chunks. Rebuilding `body_range` from what came back would
        // otherwise let a partial body read as a whole definition.
        let sought = u32::from(body_end >= symbol_range.start)
            * (body_end
                .saturating_sub(symbol_range.start)
                .saturating_add(1));
        let missing = (sought as usize).saturating_sub(body.len());
        if missing > 0 {
            context.caveats.push(format!(
                "the definition body is incomplete: {missing} of {sought} line(s) sought in {path_label}:{}-{} are outside every indexed chunk, so the returned body covers only {}-{}",
                symbol_range.start, body_end, body_range.start, body_range.end
            ));
        }
        if derived_end {
            context.caveats.push(format!(
                "the indexed range for `{}` is a single line, so the body extent comes from the chunk boundary at line {} and may include text that follows the definition",
                context.symbol.qualified_name, covering.range.end
            ));
        }
        context.body = Some(join_lines(&body));
        context.body_range = Some(body_range.clone());

        if surrounding_lines > 0 {
            let leading = indexed_lines(
                &chunks,
                body_range.start.saturating_sub(surrounding_lines).max(1),
                body_range.start.saturating_sub(1),
            );
            if leading.is_empty() && body_range.start > 1 {
                context.caveats.push(format!(
                    "no indexed lines precede {} line {}, so any documentation comment above the definition is outside the indexed corpus and is not reported",
                    path_label,
                    body_range.start
                ));
            } else if !leading.is_empty() {
                context.leading_range = Some(LineRange {
                    start: leading[0].0,
                    end: leading[leading.len() - 1].0,
                });
                context.evidence.push(format!(
                    "{} indexed line(s) above the definition were returned verbatim; documentation comments appear here only when the indexer chunked them",
                    leading.len()
                ));
                context.leading_lines = leading.into_iter().map(|(_, text)| text).collect();
            }

            let trailing = indexed_lines(
                &chunks,
                body_range.end.saturating_add(1),
                body_range.end.saturating_add(surrounding_lines),
            );
            if !trailing.is_empty() {
                context.trailing_range = Some(LineRange {
                    start: trailing[0].0,
                    end: trailing[trailing.len() - 1].0,
                });
                context.trailing_lines = trailing.into_iter().map(|(_, text)| text).collect();
            }
        }

        Ok(context)
    }

    pub fn references(&self, query: &str, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let symbol = self.definition(query)?;
        let refs = self.store.references_for_symbol(&symbol.id, limit)?;
        if !refs.is_empty() {
            return Ok(refs);
        }
        self.lexical_references(&symbol, limit)
    }

    fn lexical_references(&self, symbol: &Symbol, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let name = &symbol.name;
        let mut occurrences = Vec::new();
        let chunks = self.store.find_chunks_containing(name, limit * 4)?;
        for chunk in chunks {
            if let Some(idx) = chunk.text.find(name) {
                let before_ok = idx == 0 || {
                    let prev_char = chunk.text[..idx].chars().next_back().unwrap();
                    !prev_char.is_alphanumeric() && prev_char != '_'
                };
                let after_ok = idx + name.len() == chunk.text.len() || {
                    let next_char = chunk.text[idx + name.len()..].chars().next().unwrap();
                    !next_char.is_alphanumeric() && next_char != '_'
                };
                if before_ok && after_ok {
                    occurrences.push(SymbolOccurrence {
                        symbol_id: symbol.id.clone(),
                        file_id: chunk.file_id.clone(),
                        range: Some(chunk.range.clone()),
                        source_range: None,
                        is_definition: false,
                        confidence: Confidence::Low,
                        provenance: EvidenceSourceType::Lexical,
                    });
                    if occurrences.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(occurrences)
    }
}

fn display_path(context: &SymbolContext) -> String {
    context
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("file:{}", context.symbol.file_id.0))
}

/// Collects the indexed text for lines `from..=to`, in order. Chunks tile the
/// file without overlapping, so a line missing from the result is a line the
/// indexer never captured — it is skipped, never padded with a blank.
fn indexed_lines(chunks: &[CodeChunk], from: u32, to: u32) -> Vec<(u32, String)> {
    if to < from {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for chunk in chunks {
        if chunk.range.end < from || chunk.range.start > to {
            continue;
        }
        for (idx, text) in chunk.text.lines().enumerate() {
            let line = chunk.range.start.saturating_add(idx as u32);
            // A chunk's stored text can outrun its declared range; trust the
            // range, so a line is never attributed past the chunk that owns it.
            if line > chunk.range.end {
                break;
            }
            if line >= from && line <= to {
                lines.push((line, text.to_string()));
            }
        }
    }
    lines.sort_by_key(|(line, _)| *line);
    lines.dedup_by_key(|(line, _)| *line);
    lines
}

fn join_lines(lines: &[(u32, String)]) -> String {
    lines
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn definition_rank(symbol: &Symbol, query: &str) -> (u8, u8, usize) {
    let exactness = if symbol.name == query {
        0
    } else if symbol.qualified_name.ends_with(&format!("::{query}")) {
        1
    } else {
        2
    };
    (
        exactness,
        symbol_kind_rank(&symbol.kind),
        symbol.qualified_name.len(),
    )
}

fn symbol_kind_rank(kind: &SymbolKind) -> u8 {
    match kind {
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface => 0,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Endpoint => 1,
        SymbolKind::Module | SymbolKind::Package => 2,
        SymbolKind::Constant | SymbolKind::Field | SymbolKind::Variable => 3,
        SymbolKind::DatabaseTable | SymbolKind::Test | SymbolKind::Unknown => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::{SymbolEngine, SYMBOL_CONTEXT_MAX_BODY_LINES, SYMBOL_CONTEXT_SURROUNDING_LINES};
    use open_kioku_core::{
        CodeChunk, File, FileId, Import, IndexManifest, Language, LineRange, RepositoryId, Symbol,
        SymbolId, SymbolKind, SymbolOccurrence, TestTarget,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{IndexData, MetadataStore};
    use std::path::Path;

    #[derive(Default)]
    struct MemoryStore {
        symbols: Vec<Symbol>,
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
            Ok(self
                .files
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok(self.files.iter().find(|file| file.path == path).cloned())
        }

        fn list_symbols(
            &self,
            query: Option<&str>,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<Symbol>> {
            let query = query.unwrap_or_default();
            Ok(self
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.name.contains(query) || symbol.qualified_name.contains(query)
                })
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        fn symbol_by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(self.symbols.iter().find(|symbol| symbol.id == *id).cloned())
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

    fn symbol(id: &str, name: &str, qualified_name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified_name.into(),
            kind,
            file_id: FileId::new("file"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: open_kioku_core::Confidence::High,
            provenance: open_kioku_core::EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        }
    }

    #[test]
    fn definition_prefers_exact_class_match_over_prefix_matches() {
        let store = MemoryStore {
            symbols: vec![
                symbol(
                    "prefix",
                    "SearchServiceCleanupOnLostMasterIT",
                    "server::SearchServiceCleanupOnLostMasterIT",
                    SymbolKind::Class,
                ),
                symbol(
                    "field",
                    "searchService",
                    "server::TransportSearchAction::searchService",
                    SymbolKind::Field,
                ),
                symbol(
                    "class",
                    "SearchService",
                    "server::search::SearchService::SearchService",
                    SymbolKind::Class,
                ),
                symbol(
                    "ctor",
                    "SearchService",
                    "server::search::SearchService::SearchService",
                    SymbolKind::Method,
                ),
            ],
            ..Default::default()
        };

        let definition = SymbolEngine::new(&store)
            .definition("SearchService")
            .unwrap();

        assert_eq!(definition.id.0, "class");
    }

    fn ranged_symbol(id: &str, name: &str, range: LineRange) -> Symbol {
        let mut symbol = symbol(id, name, &format!("billing::{name}"), SymbolKind::Function);
        symbol.file_id = FileId::new("file-billing");
        symbol.range = Some(range);
        symbol
    }

    fn billing_file() -> File {
        File {
            id: FileId::new("file-billing"),
            repository_id: RepositoryId::new("repo"),
            path: "src/billing.rs".into(),
            language: Language::Rust,
            size_bytes: 128,
            content_hash: "hash-billing".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn chunk(id: &str, symbol_id: &str, start: u32, text: &str) -> CodeChunk {
        let end = start + text.lines().count().saturating_sub(1) as u32;
        CodeChunk {
            id: id.into(),
            file_id: FileId::new("file-billing"),
            range: LineRange { start, end },
            language: Language::Rust,
            text: text.into(),
            symbol_id: Some(SymbolId::new(symbol_id)),
        }
    }

    /// Two definitions, the second carrying a doc comment. The indexer cuts the
    /// first chunk at the second symbol's line, so that doc comment sits at the
    /// tail of the *previous* chunk and is recoverable as leading context.
    fn documented_store() -> MemoryStore {
        MemoryStore {
            symbols: vec![
                ranged_symbol("symbol-publish", "publish_invoice", LineRange::single(3)),
                ranged_symbol(
                    "symbol-archive",
                    "archive_invoice",
                    LineRange { start: 9, end: 11 },
                ),
            ],
            files: vec![billing_file()],
            chunks: vec![
                chunk(
                    "chunk-publish",
                    "symbol-publish",
                    3,
                    "pub fn publish_invoice() {\n    emit();\n}\n\n/// Archives a published invoice.\n/// Returns the archived id.",
                ),
                chunk(
                    "chunk-archive",
                    "symbol-archive",
                    9,
                    "pub fn archive_invoice() -> u64 {\n    0\n}",
                ),
            ],
        }
    }

    #[test]
    fn context_returns_the_definition_body_at_absolute_line_numbers() {
        let store = documented_store();

        let context = SymbolEngine::new(&store)
            .context("archive_invoice", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert_eq!(context.path.as_deref(), Some(Path::new("src/billing.rs")));
        assert_eq!(
            context.body.as_deref(),
            Some("pub fn archive_invoice() -> u64 {\n    0\n}")
        );
        assert_eq!(context.body_range, Some(LineRange { start: 9, end: 11 }));
        assert!(!context.truncated);
        assert!(context
            .evidence
            .iter()
            .any(|entry| entry.contains("chunk-archive")));
    }

    #[test]
    fn context_returns_doc_comment_lines_when_the_index_captured_them() {
        let store = documented_store();

        let context = SymbolEngine::new(&store)
            .context("archive_invoice", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert_eq!(
            context.leading_lines,
            vec![
                "pub fn publish_invoice() {".to_string(),
                "    emit();".to_string(),
                "}".to_string(),
                String::new(),
                "/// Archives a published invoice.".to_string(),
                "/// Returns the archived id.".to_string(),
            ]
        );
        assert_eq!(context.leading_range, Some(LineRange { start: 3, end: 8 }));
    }

    #[test]
    fn context_reports_that_lines_above_the_first_indexed_symbol_are_not_in_the_corpus() {
        let store = documented_store();

        let context = SymbolEngine::new(&store)
            .context("publish_invoice", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert!(context.leading_lines.is_empty());
        assert!(
            context
                .caveats
                .iter()
                .any(|caveat| caveat.contains("documentation comment")),
            "absent leading context must be reported, got: {:?}",
            context.caveats
        );
        // The symbol's own range is one line, so the extent is a chunk boundary
        // and the bundle has to say so.
        assert!(context
            .caveats
            .iter()
            .any(|caveat| caveat.contains("chunk boundary")));
    }

    #[test]
    fn context_reports_an_unrecoverable_body_instead_of_returning_less_than_promised() {
        let store = MemoryStore {
            symbols: vec![ranged_symbol(
                "symbol-orphan",
                "orphan_symbol",
                LineRange::single(42),
            )],
            files: vec![billing_file()],
            chunks: Vec::new(),
        };

        let context = SymbolEngine::new(&store)
            .context("orphan_symbol", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert!(context.body.is_none());
        assert!(context.body_range.is_none());
        assert!(
            context
                .caveats
                .iter()
                .any(|caveat| caveat.contains("no indexed chunk covers")),
            "missing body must be reported, got: {:?}",
            context.caveats
        );
    }

    /// The fixture that shipped with this feature is itself an instance: a chunk
    /// declaring lines 7-9 but storing one line. Rebuilding `body_range` from what
    /// came back would let that read as a whole definition.
    #[test]
    fn context_reports_a_body_recovered_only_in_part() {
        let store = MemoryStore {
            symbols: vec![ranged_symbol(
                "symbol-sparse",
                "sparse_symbol",
                LineRange::single(7),
            )],
            files: vec![billing_file()],
            chunks: vec![CodeChunk {
                id: "chunk-sparse".into(),
                file_id: FileId::new("file-billing"),
                range: LineRange { start: 7, end: 9 },
                language: Language::Rust,
                text: "pub fn sparse_symbol() {}".into(),
                symbol_id: Some(SymbolId::new("symbol-sparse")),
            }],
        };

        let context = SymbolEngine::new(&store)
            .context("sparse_symbol", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert_eq!(context.body.as_deref(), Some("pub fn sparse_symbol() {}"));
        assert_eq!(context.body_range, Some(LineRange { start: 7, end: 7 }));
        assert!(
            context
                .caveats
                .iter()
                .any(|caveat| caveat.contains("body is incomplete") && caveat.contains("2 of 3")),
            "a short body must name the lines it could not recover, got: {:?}",
            context.caveats
        );
        // The boundary caveat names the chunk boundary, not the last line that
        // happened to come back; the evidence string must agree with it.
        assert!(
            context
                .caveats
                .iter()
                .any(|caveat| caveat.contains("chunk boundary at line 9")),
            "the boundary caveat must name the chunk boundary, got: {:?}",
            context.caveats
        );
        assert!(context
            .evidence
            .iter()
            .any(|entry| entry.contains("lines 7-9")));
    }

    /// A definition on line 1 has no preamble, so there is no gap to report.
    #[test]
    fn context_does_not_invent_a_missing_preamble_for_a_definition_on_line_one() {
        let store = MemoryStore {
            symbols: vec![ranged_symbol(
                "symbol-first",
                "first_symbol",
                LineRange { start: 1, end: 2 },
            )],
            files: vec![billing_file()],
            chunks: vec![chunk(
                "chunk-first",
                "symbol-first",
                1,
                "pub fn first_symbol() {}\nlet started = true;",
            )],
        };

        let context = SymbolEngine::new(&store)
            .context("first_symbol", SYMBOL_CONTEXT_SURROUNDING_LINES)
            .unwrap();

        assert!(context.leading_lines.is_empty());
        assert!(
            !context
                .caveats
                .iter()
                .any(|caveat| caveat.contains("documentation comment")),
            "line 1 has no preamble to be missing, got: {:?}",
            context.caveats
        );
    }

    #[test]
    fn context_bounds_the_body_and_says_that_it_was_cut() {
        let long_body = (0..(SYMBOL_CONTEXT_MAX_BODY_LINES + 50))
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let store = MemoryStore {
            symbols: vec![ranged_symbol(
                "symbol-long",
                "long_symbol",
                LineRange::single(1),
            )],
            files: vec![billing_file()],
            chunks: vec![chunk("chunk-long", "symbol-long", 1, &long_body)],
        };

        let context = SymbolEngine::new(&store).context("long_symbol", 0).unwrap();

        assert!(context.truncated);
        assert_eq!(
            context.body_range,
            Some(LineRange {
                start: 1,
                end: SYMBOL_CONTEXT_MAX_BODY_LINES
            })
        );
        assert!(context
            .caveats
            .iter()
            .any(|caveat| caveat.contains("was cut at")));
    }
}
