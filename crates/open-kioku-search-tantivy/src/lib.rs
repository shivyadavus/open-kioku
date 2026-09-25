use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, GraphNode, LineRange, ScoreComponent,
    SearchResult, Symbol,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::EvidenceBuilder;
use open_kioku_storage::SearchIndex;
use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::index::SegmentId;
use tantivy::indexer::NoMergePolicy;
use tantivy::query::{Query, QueryParser};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value, FAST,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{doc, DocAddress, DocId, Index, IndexWriter, Score, Searcher, SegmentReader};

/// Indexing memory ceiling, the same total the multi-threaded writer was given.
const WRITER_MEMORY_BUDGET: usize = 50_000_000;

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
    /// Absent from indexes written before documents carried a repository-order rank.
    order_key: Option<Field>,
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
        // Tokenizers are not persisted with the index; every open must register ours before
        // the schema (old or new) can resolve it by name.
        index
            .tokenizers()
            .register(CODE_TOKENIZER, CodeIdentifierTokenizer);
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
        self.write_documents(chunks, files, symbols, graph_nodes, WRITER_MEMORY_BUDGET)?;
        Ok(())
    }

    /// Writes every document into one segment, in repository order, from one indexing thread,
    /// and returns how many segments the thread flushed before they were merged. A document's
    /// address in that segment is its repository-order rank.
    ///
    /// BM25 statistics are summed across segments, but a document's score is summed across the
    /// query's terms in an order that follows how the pruning scorer walked that segment's
    /// postings, so it depends on which documents share the segment and in what order. With
    /// Tantivy's default writer that layout came from how its indexing threads split the input,
    /// and two indexes of one tree scored the same document a few ULP apart (#491), enough for
    /// near-tied candidates to compare unequal and bypass the tie-breaks. A single thread fed in
    /// repository order, with no background merges and one deterministic final merge, fixes the
    /// layout: document ids follow repository order whatever order the caller passed.
    fn write_documents(
        &mut self,
        chunks: &[CodeChunk],
        files: &[File],
        symbols: &[Symbol],
        graph_nodes: &[GraphNode],
        memory_budget: usize,
    ) -> Result<usize> {
        let mut writer: IndexWriter = self
            .index
            .writer_with_num_threads(1, memory_budget)
            .map_err(search_err)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        writer.delete_all_documents().map_err(search_err)?;
        let files_by_id = files
            .iter()
            .map(|file| (file.id.0.as_str(), file))
            .collect::<HashMap<_, _>>();
        let symbols_by_id = symbols
            .iter()
            .map(|symbol| (symbol.id.0.as_str(), symbol))
            .collect::<HashMap<_, _>>();
        let order = document_order(chunks, graph_nodes, &files_by_id, &symbols_by_id);
        for (rank, &entry) in order.iter().enumerate() {
            let mut document = match entry {
                DocumentEntry::Chunk(index) => {
                    let chunk = &chunks[index];
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
                    doc!(
                        self.fields.path => file.path.to_string_lossy().to_string(),
                        self.fields.content => format!("{}\n{}", file.path.display(), chunk.text),
                        self.fields.chunk_json => serde_json::to_string(chunk)?,
                        self.fields.file_json => serde_json::to_string(file)?,
                        self.fields.symbol_json => symbol_json,
                    )
                }
                DocumentEntry::GraphNode(index) => {
                    let node = &graph_nodes[index];
                    let Some(file) = graph_node_file(node, &files_by_id, &symbols_by_id) else {
                        continue;
                    };
                    let symbol = node
                        .symbol_id
                        .as_ref()
                        .and_then(|id| symbols_by_id.get(id.0.as_str()).copied());
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
                    doc!(
                        self.fields.path => file.path.to_string_lossy().to_string(),
                        self.fields.content => graph_chunk.text.clone(),
                        self.fields.chunk_json => serde_json::to_string(&graph_chunk)?,
                        self.fields.file_json => serde_json::to_string(file)?,
                        self.fields.symbol_json => symbol_json,
                    )
                }
            };
            if let Some(order_key) = self.fields.order_key {
                document.add_u64(order_key, rank as u64);
            }
            writer.add_document(document).map_err(search_err)?;
        }
        writer.commit().map_err(search_err)?;
        // Past the memory budget the one thread flushes more than one segment; each holds a
        // contiguous run of the repository order, so stacking them by their first document's
        // rank reproduces it exactly. Tantivy lists segments in no particular order.
        let segments = self.segments_in_repository_order()?;
        if segments.len() > 1 {
            writer.merge(&segments).wait().map_err(search_err)?;
        }
        writer.wait_merging_threads().map_err(search_err)?;
        Ok(segments.len())
    }

    fn segments_in_repository_order(&self) -> Result<Vec<SegmentId>> {
        let reader = self.index.reader().map_err(search_err)?;
        let searcher = reader.searcher();
        let mut segments = searcher
            .segment_readers()
            .iter()
            .map(|segment_reader| {
                // An index written before the order key existed has nothing to order segments
                // by; it is rebuilt with the current schema by the next `ok index`.
                let first_rank = segment_reader
                    .fast_fields()
                    .u64(ORDER_KEY_FIELD)
                    .ok()
                    .and_then(|column| column.first(0))
                    .unwrap_or(u64::MAX);
                (first_rank, segment_reader.segment_id())
            })
            .collect::<Vec<_>>();
        segments.sort_unstable();
        Ok(segments.into_iter().map(|(_, id)| id).collect())
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
            let top_docs = self.top_docs(&searcher, &query, limit.saturating_mul(4).max(limit))?;
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
                    exact_reference_provenance: None,
                });
            }
        }
        results.sort_by(compare_search_results);
        results.truncate(limit);
        Ok(results)
    }

    /// The best `limit` documents by score, breaking equal scores on repository order.
    fn top_docs(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        limit: usize,
    ) -> Result<Vec<(Score, DocAddress)>> {
        // One hit past the limit shows whether the limit falls inside a run of equal scores.
        // When it does not, the kept set is the same whichever tied documents the pruned score
        // collector visited first, and `compare_search_results` orders them afterwards.
        let mut hits = searcher
            .search(query, &TopDocs::with_limit(limit + 1).order_by_score())
            .map_err(search_err)?;
        let limit_splits_a_tie =
            limit > 0 && hits.len() > limit && hits[limit - 1].0 <= hits[limit].0;
        if !limit_splits_a_tie || self.fields.order_key.is_none() {
            // An index written before the order key existed keeps Tantivy's address tie-break
            // until the next `ok index` rebuilds it with the current schema.
            hits.truncate(limit);
            return Ok(hits);
        }
        // Scoring every match on the order key costs more than pruned collection, so it is
        // reserved for the queries whose result set a tie would otherwise decide.
        let collector = TopDocs::with_limit(limit).tweak_score(|segment_reader: &SegmentReader| {
            let order = segment_reader.fast_fields().u64(ORDER_KEY_FIELD).ok();
            move |doc: DocId, score: Score| {
                let rank = order
                    .as_ref()
                    .and_then(|column| column.first(doc))
                    .unwrap_or(u64::MAX);
                // The collector keeps the largest keys, so the rank is reversed: on equal
                // scores the document earlier in repository order wins.
                (score, Reverse(rank))
            }
        });
        Ok(searcher
            .search(query, &collector)
            .map_err(search_err)?
            .into_iter()
            .map(|((score, _), address)| (score, address))
            .collect())
    }
}

/// Descending score, then repository position. Search results that tie on score come back in
/// the same order from every index of the same tree.
fn compare_search_results(left: &SearchResult, right: &SearchResult) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| line_range_bounds(left).cmp(&line_range_bounds(right)))
        .then_with(|| left.evidence_refs.cmp(&right.evidence_refs))
}

fn line_range_bounds(result: &SearchResult) -> Option<(u32, u32)> {
    result
        .line_range
        .as_ref()
        .map(|range| (range.start, range.end))
}

/// Index-writing order: code chunks, then graph nodes, each by path, line range and id. A
/// document's position in it is its rank, indexed as a fast field that the collector breaks
/// equal scores on when the result limit falls between them.
///
/// Tantivy itself breaks them by document address, which followed how its indexing threads
/// split documents into segments rather than anything in the repository. Re-indexing one tree
/// could therefore reorder tied search results, and through rank fusion the context paths
/// built from them (#468). Documents without a file are left out.
fn document_order(
    chunks: &[CodeChunk],
    graph_nodes: &[GraphNode],
    files_by_id: &HashMap<&str, &File>,
    symbols_by_id: &HashMap<&str, &Symbol>,
) -> Vec<DocumentEntry> {
    let mut order = Vec::with_capacity(chunks.len() + graph_nodes.len());
    for (index, chunk) in chunks.iter().enumerate() {
        if let Some(file) = files_by_id.get(chunk.file_id.0.as_str()) {
            order.push((
                0u8,
                file.path.as_path(),
                chunk.range.start,
                chunk.range.end,
                chunk.id.as_str(),
                DocumentEntry::Chunk(index),
            ));
        }
    }
    for (index, node) in graph_nodes.iter().enumerate() {
        if let Some(file) = graph_node_file(node, files_by_id, symbols_by_id) {
            let range = node
                .symbol_id
                .as_ref()
                .and_then(|id| symbols_by_id.get(id.0.as_str()))
                .and_then(|symbol| symbol.range.as_ref());
            order.push((
                1u8,
                file.path.as_path(),
                range.map_or(1, |range| range.start),
                range.map_or(1, |range| range.end),
                node.id.0.as_str(),
                DocumentEntry::GraphNode(index),
            ));
        }
    }
    order.sort_unstable();
    order.into_iter().map(|entry| entry.5).collect()
}

/// A document to index, by its position in the caller's chunk or graph-node slice.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DocumentEntry {
    Chunk(usize),
    GraphNode(usize),
}

/// The file a graph node is indexed under: its own, or else its symbol's.
fn graph_node_file<'a>(
    node: &GraphNode,
    files_by_id: &HashMap<&str, &'a File>,
    symbols_by_id: &HashMap<&str, &Symbol>,
) -> Option<&'a File> {
    node.file_id
        .as_ref()
        .and_then(|id| files_by_id.get(id.0.as_str()).copied())
        .or_else(|| {
            node.symbol_id.as_ref().and_then(|id| {
                symbols_by_id
                    .get(id.0.as_str())
                    .and_then(|symbol| files_by_id.get(symbol.file_id.0.as_str()).copied())
            })
        })
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
    // Resolves through the active index generation when one is published (RI3.6);
    // legacy layouts keep the historical path.
    open_kioku_storage::generations::resolve_index_location(repo.as_ref()).tantivy_dir()
}

fn schema() -> Schema {
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("default")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    // Code text is indexed whole and as identifier parts. The path field deliberately is not:
    // measured on the 490-case commit-derived corpus, parts in content lifted MRR from 0.235 to
    // 0.393 (dev) / 0.202 to 0.337 (holdout), while also splitting paths lowered both again
    // (0.381 / 0.330) by letting a short file name that shares a part outrank a whole-word
    // match in content.
    let code_text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(CODE_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let stored_text = TextOptions::default().set_stored();
    let mut builder = Schema::builder();
    builder.add_text_field("path", text);
    builder.add_text_field("content", code_text.clone());
    builder.add_text_field("chunk_json", stored_text.clone());
    builder.add_text_field("file_json", stored_text.clone());
    builder.add_text_field("symbol_json", code_text);
    builder.add_u64_field(ORDER_KEY_FIELD, FAST);
    builder.build()
}

/// Fast field holding each document's rank in repository order; see `document_order_ranks`.
const ORDER_KEY_FIELD: &str = "order_key";

/// Name of the identifier-aware tokenizer used by code text fields.
///
/// Indexes built before it existed carry `default` in their on-disk schema and keep working
/// unchanged; the next `ok index` rebuild switches them over.
const CODE_TOKENIZER: &str = "code_identifiers";
const MAX_TOKEN_LEN: usize = 40;

/// Tokenizer that indexes each identifier both whole and as its CamelCase / snake_case parts,
/// emitted at the same position so phrase queries still line up.
///
/// `SlotPlanner` becomes `slotplanner`, `slot`, `planner`; `min_free_blocks` becomes `min`,
/// `free`, `blocks`. Without this a prose task like "slot planner" cannot reach
/// `SlotPlanner.java` at all: the default tokenizer only knows the whole word, and query-side
/// splitting (which we already do) cannot recover parts the index never stored. On the
/// identifier-heavy Java corpus this is the single largest lexical lever measured in the
/// literature (+28% NDCG@10 for Java, +82% for Go, ~0 for Python, which snake_case already
/// splits).
#[derive(Clone, Default)]
struct CodeIdentifierTokenizer;

struct CodeIdentifierTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for CodeIdentifierTokenizer {
    type TokenStream<'a> = CodeIdentifierTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CodeIdentifierTokenStream {
            tokens: code_identifier_tokens(text),
            index: usize::MAX,
        }
    }
}

impl TokenStream for CodeIdentifierTokenStream {
    fn advance(&mut self) -> bool {
        self.index = self.index.wrapping_add(1);
        self.index < self.tokens.len()
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index]
    }
}

fn code_identifier_tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0usize;
    let mut start: Option<usize> = None;
    let emit = |start: usize, end: usize, position: usize, tokens: &mut Vec<Token>| {
        let word = &text[start..end];
        // Same cap as tantivy's default analyzer: minified blobs and hashes are not words.
        if word.len() > MAX_TOKEN_LEN {
            return;
        }
        let whole = word.to_lowercase();
        tokens.push(Token {
            offset_from: start,
            offset_to: end,
            position,
            text: whole.clone(),
            position_length: 1,
        });
        let parts = if word.is_ascii() {
            split_identifier(word)
        } else {
            Vec::new()
        };
        if parts.len() > 1 {
            for part in parts {
                if part != whole {
                    tokens.push(Token {
                        offset_from: start,
                        offset_to: end,
                        position,
                        text: part,
                        position_length: 1,
                    });
                }
            }
        }
    };
    for (offset, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(offset);
            }
        } else if let Some(begin) = start.take() {
            emit(begin, offset, position, &mut tokens);
            position += 1;
        }
    }
    if let Some(begin) = start {
        emit(begin, text.len(), position, &mut tokens);
    }
    tokens
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
        order_key: schema.get_field(ORDER_KEY_FIELD).ok(),
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
mod order_tests {
    use super::{compare_search_results, rebuild_disk_index, TantivySearchIndex, ORDER_KEY_FIELD};
    use open_kioku_core::{
        CodeChunk, File, FileId, Language, LineRange, RepositoryId, SearchResult,
    };
    use open_kioku_storage::SearchIndex;

    /// `count` files whose single chunk is identical, so every search score ties.
    fn tied_fixture(count: usize) -> (Vec<File>, Vec<CodeChunk>) {
        let files = (0..count)
            .map(|index| File {
                id: FileId::new(format!("file-{index:02}")),
                repository_id: RepositoryId::new("repo"),
                path: format!("src/widget_{index:02}.rs").into(),
                language: Language::Rust,
                size_bytes: 64,
                content_hash: format!("hash-{index:02}"),
                is_generated: false,
                is_vendor: false,
            })
            .collect::<Vec<_>>();
        let chunks = files
            .iter()
            .map(|file| CodeChunk {
                id: format!("chunk-{}", file.id.0),
                file_id: file.id.clone(),
                range: LineRange { start: 1, end: 3 },
                language: Language::Rust,
                text: "pub fn render() -> u32 {\n    frame\n}".into(),
                symbol_id: None,
            })
            .collect::<Vec<_>>();
        (files, chunks)
    }

    fn result(path: &str, start: u32, score: f32) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: Some(LineRange {
                start,
                end: start + 2,
            }),
            snippet: String::new(),
            symbol: None,
            score,
            match_reason: String::new(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        }
    }

    #[test]
    fn search_results_break_equal_scores_on_path_then_line_range() {
        let inputs = vec![
            result("src/b.rs", 30, 1.0),
            result("src/c.rs", 1, 2.0),
            result("src/b.rs", 4, 1.0),
            result("src/a.rs", 90, 1.0),
        ];
        let mut reversed = inputs.clone();
        reversed.reverse();
        for mut results in [inputs, reversed] {
            results.sort_by(compare_search_results);
            let order = results
                .iter()
                .map(|result| {
                    (
                        result.path.to_string_lossy().into_owned(),
                        result.line_range.as_ref().map(|range| range.start),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                order,
                vec![
                    ("src/c.rs".to_string(), Some(1)),
                    ("src/a.rs".to_string(), Some(90)),
                    ("src/b.rs".to_string(), Some(4)),
                    ("src/b.rs".to_string(), Some(30)),
                ]
            );
        }
    }

    #[test]
    fn tied_documents_are_collected_in_repository_order_whatever_the_indexing_order() {
        // Twenty tied documents and a limit of two: the collector keeps eight, so which eight
        // it keeps, not only their order, has to follow the repository.
        let (files, chunks) = tied_fixture(20);
        let mut reversed = chunks.clone();
        reversed.reverse();
        for chunks in [chunks, reversed] {
            let temp = tempfile::tempdir().unwrap();
            let index = rebuild_disk_index(temp.path(), &chunks, &files, &[]).unwrap();
            let paths = index
                .search("render", 2)
                .unwrap()
                .into_iter()
                .map(|result| result.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(paths, vec!["src/widget_00.rs", "src/widget_01.rs"]);
        }
    }

    #[test]
    fn an_index_written_without_the_order_key_still_searches() {
        let temp = tempfile::tempdir().unwrap();
        let mut builder = tantivy::schema::Schema::builder();
        for (_, entry) in super::schema().fields() {
            if entry.name() != ORDER_KEY_FIELD {
                builder.add_field(entry.clone());
            }
        }
        tantivy::Index::create_in_dir(temp.path(), builder.build()).unwrap();
        let mut index = TantivySearchIndex::open_or_create(temp.path()).unwrap();
        let (files, chunks) = tied_fixture(3);
        index.rebuild(&chunks, &files, &[]).unwrap();
        assert_eq!(index.search("render", 5).unwrap().len(), 3);
    }
}

#[cfg(test)]
mod tokenizer_tests {
    use super::code_identifier_tokens;

    fn texts(input: &str) -> Vec<(String, usize)> {
        code_identifier_tokens(input)
            .into_iter()
            .map(|token| (token.text, token.position))
            .collect()
    }

    #[test]
    fn identifiers_are_indexed_whole_and_as_parts_at_one_position() {
        assert_eq!(
            texts("SlotPlanner min_free_blocks"),
            vec![
                ("slotplanner".into(), 0),
                ("slot".into(), 0),
                ("planner".into(), 0),
                ("min".into(), 1),
                ("free".into(), 2),
                ("blocks".into(), 3),
            ]
        );
    }

    #[test]
    fn plain_words_acronyms_and_paths_behave_like_the_default_tokenizer() {
        assert_eq!(
            texts("plugins/rate-limit/RateLimitFilter.java"),
            vec![
                ("plugins".into(), 0),
                ("rate".into(), 1),
                ("limit".into(), 2),
                ("ratelimitfilter".into(), 3),
                ("rate".into(), 3),
                ("limit".into(), 3),
                ("filter".into(), 3),
                ("java".into(), 4),
            ]
        );
        assert_eq!(texts("HTTP"), vec![("http".into(), 0)]);
        assert_eq!(
            texts("Ünïcode wörd"),
            vec![("ünïcode".into(), 0), ("wörd".into(), 1)]
        );
    }

    #[test]
    fn overlong_blobs_are_dropped() {
        let blob = "a".repeat(41);
        assert!(texts(&format!("{blob} ok"))
            .iter()
            .all(|(text, _)| text == "ok"));
    }
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
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
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

#[cfg(test)]
mod determinism_tests {
    use super::{rebuild_disk_index, TantivySearchIndex};
    use open_kioku_core::{CodeChunk, File, FileId, Language, LineRange, RepositoryId};

    /// Tantivy's per-thread floor (`MEMORY_BUDGET_NUM_BYTES_MIN`, which it does not export).
    const SMALLEST_MEMORY_BUDGET: usize = 15_000_000;

    const VOCABULARY: &str = "cache render frame token retry import graph node edge path slot \
        planner buffer queue worker index segment score query symbol reference module crate parser \
        writer reader commit merge policy budget thread evidence boundary context ranking fusion";

    /// Queries of three to five terms, so a document's score sums several per-term
    /// contributions and the order of that summation shows up in the low bits.
    const QUERIES: &[&str] = &[
        "cache render frame",
        "retry import token queue",
        "graph node edge path segment",
        "slot planner buffer",
        "worker index segment score query",
        "symbol reference module crate",
        "parser writer reader commit merge",
        "policy budget thread evidence",
        "boundary context ranking fusion cache",
    ];

    /// A few hundred chunks of pseudo-random identifiers, generated from a fixed seed so the
    /// corpus is the same on every platform.
    fn corpus(
        file_count: usize,
        max_words: usize,
        rare_words: usize,
    ) -> (Vec<File>, Vec<CodeChunk>) {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let vocabulary = VOCABULARY.split_whitespace().collect::<Vec<_>>();
        let mut next = move |bound: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as usize) % bound
        };
        let mut files = Vec::new();
        let mut chunks = Vec::new();
        for file_index in 0..file_count {
            let file = File {
                id: FileId::new(format!("file-{file_index:03}")),
                repository_id: RepositoryId::new("repo"),
                path: format!("src/module_{file_index:03}.rs").into(),
                language: Language::Rust,
                size_bytes: 512,
                content_hash: format!("hash-{file_index:03}"),
                is_generated: false,
                is_vendor: false,
            };
            for chunk_index in 0..6u32 {
                let words = 8 + next(max_words);
                let mut text = (0..words)
                    .map(|_| vocabulary[next(vocabulary.len())].to_string())
                    .collect::<Vec<_>>();
                // Words no query asks for, which fill the indexing arena quickly.
                text.extend((0..rare_words).map(|_| format!("w{}", next(1_000_000))));
                let text = text.join(" ");
                chunks.push(CodeChunk {
                    id: format!("chunk-{file_index:03}-{chunk_index}"),
                    file_id: file.id.clone(),
                    range: LineRange {
                        start: chunk_index * 10 + 1,
                        end: chunk_index * 10 + 9,
                    },
                    language: Language::Rust,
                    text,
                    symbol_id: None,
                });
            }
            files.push(file);
        }
        (files, chunks)
    }

    /// Every result's identity and the raw bits of the scores it carries, per query.
    fn fingerprint(index: &TantivySearchIndex) -> Vec<Vec<(String, u32, u32, u32)>> {
        QUERIES
            .iter()
            .map(|query| {
                index
                    .search_all(query, 40)
                    .unwrap()
                    .into_iter()
                    .map(|result| {
                        let bm25 = result
                            .score_breakdown
                            .iter()
                            .find(|component| component.signal == "bm25_relevance")
                            .map(|component| component.raw_value)
                            .unwrap();
                        (
                            result.path.to_string_lossy().into_owned(),
                            result.line_range.map_or(0, |range| range.start),
                            bm25.to_bits(),
                            result.score.to_bits(),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    /// Each document's repository-order rank, by document address. Ranks equal to addresses
    /// mean the merged segment stacked the flushed ones in the order they were written.
    fn document_ranks(index: &TantivySearchIndex) -> Vec<Vec<Option<u64>>> {
        let searcher = index.index.reader().unwrap().searcher();
        searcher
            .segment_readers()
            .iter()
            .map(|segment| {
                let column = segment.fast_fields().u64(super::ORDER_KEY_FIELD).unwrap();
                (0..segment.max_doc())
                    .map(|doc| column.first(doc))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn scores_are_bit_identical_whatever_the_indexing_order() {
        let (files, chunks) = corpus(60, 40, 0);
        let mut reversed = chunks.clone();
        reversed.reverse();
        let forward_dir = tempfile::tempdir().unwrap();
        let reversed_dir = tempfile::tempdir().unwrap();
        let forward = rebuild_disk_index(forward_dir.path(), &chunks, &files, &[]).unwrap();
        let reversed = rebuild_disk_index(reversed_dir.path(), &reversed, &files, &[]).unwrap();
        let forward = fingerprint(&forward);
        assert!(forward.iter().all(|results| results.len() > 10));
        // Before #491 was fixed, reversing the input moved some scores by one or two ULP and
        // reordered the results of two of these queries.
        assert_eq!(forward, fingerprint(&reversed));
    }

    #[test]
    fn scores_do_not_depend_on_how_many_segments_the_writer_flushed() {
        // Enough text to overflow the smallest indexing budget Tantivy accepts, so one index is
        // flushed as several segments and merged while the other is written as one.
        let (files, chunks) = corpus(40, 20, 150);
        let write = |memory_budget: usize| {
            let dir = tempfile::tempdir().unwrap();
            let mut index = TantivySearchIndex::open_or_create(dir.path()).unwrap();
            let flushed = index
                .write_documents(&chunks, &files, &[], &[], memory_budget)
                .unwrap();
            let segments = index.index.searchable_segment_ids().unwrap().len();
            (dir, index, flushed, segments)
        };
        let (_split_dir, split, split_flushed, split_segments) = write(SMALLEST_MEMORY_BUDGET);
        let (_whole_dir, whole, whole_flushed, whole_segments) = write(super::WRITER_MEMORY_BUDGET);
        assert!(split_flushed > 1, "flushed {split_flushed} segment(s)");
        assert_eq!(whole_flushed, 1);
        assert_eq!((split_segments, whole_segments), (1, 1));
        assert_eq!(document_ranks(&split), document_ranks(&whole));
        assert_eq!(fingerprint(&split), fingerprint(&whole));
    }
}
