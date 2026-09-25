//! Ranked code search: the one path both `ok search` and MCP `search_code` answer through.
//!
//! The surfaces used to rank the same query differently. The CLI reranked the index's
//! candidates with the repository's `[ranking]` weights, while `search_code` returned the
//! index's own order (#448). Everything that decides which results a page holds, and in what
//! order, lives here: where candidates come from, how deep they are fetched, the co-change
//! annotation, the rerank, per-path deduplication and the page slice. A ranking change made
//! here reaches both surfaces at once. A surface chooses only where the `[ranking]` weights and
//! the semantic index come from, and how the page is rendered.

use crate::evidence_pairs::{merge_evidence, pair_evidence_refs, push_evidence};
use open_kioku_config::RankingConfig;
use open_kioku_core::{AnalysisFact, EvidenceSourceType, FileId, ScoreComponent, SearchResult};
use open_kioku_errors::{OkError, Result};
use open_kioku_ranking::{
    rerank_with_options, RankingMode, RankingOptions, RankingWeights, TextRelevanceScale,
};
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{default_index_dir, TantivySearchIndex};
use open_kioku_storage::{require_search_query, MetadataStore, SearchIndex};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `match_reason` of a candidate that local git history added, rather than the query found.
pub const HISTORICAL_COCHANGE_REASON: &str = "historical git co-change candidate";

/// The most candidates one source is asked for. It is the depth MCP `search_code` could page
/// through before both surfaces shared this ranking, so no page is fetched shallower than it was.
pub const MAX_CANDIDATE_DEPTH: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Lexical candidates from the Tantivy index, or a scan of the stored chunks without one.
    Code,
    /// Indexed graph-node documents, in the index's order. Neither surface has reranked them.
    Graph,
    /// Candidates from the local semantic vector index.
    Semantic,
    /// Lexical and semantic candidates ranked together.
    Hybrid,
}

/// A semantic vector index as a candidate source. The surface owns the index and its
/// configuration; ranking only needs to ask it for candidates.
pub struct SemanticCandidates<'a> {
    /// Whether the index can answer. Hybrid mode leaves semantic candidates out when it cannot.
    /// Semantic mode asks regardless, so the index's own refusal, which names the command that
    /// builds it, reaches the caller.
    pub ready: bool,
    pub search: &'a dyn Fn(&str, usize) -> Result<Vec<SearchResult>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RankedSearchRequest<'a> {
    pub query: &'a str,
    pub mode: SearchMode,
    /// Results per page. `None` returns every unique path in the candidate pool, which is what
    /// `ok search --limit 0` has always printed.
    pub limit: Option<usize>,
    pub offset: usize,
}

impl RankedSearchRequest<'_> {
    fn page_end(&self) -> usize {
        self.offset.saturating_add(self.limit.unwrap_or(0))
    }

    /// Unique results the page needs: through its end and one more, so `has_more` can be told
    /// apart from the end of the ranking.
    fn window(&self) -> usize {
        match self.limit {
            Some(_) => self.page_end().saturating_add(1),
            None => usize::MAX,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RankedSearchPage {
    /// Results `offset..offset + limit` of the ranking.
    pub results: Vec<SearchResult>,
    /// At least one ranked result follows this page.
    pub has_more: bool,
    /// How many candidates each source was asked for.
    pub candidate_depth: usize,
    /// A source returned every candidate it was asked for, so the index may hold matches this
    /// ranking never saw. It matters only on a page that ends the results: paging further
    /// cannot reach them, and the caller should say so rather than imply the list is complete.
    pub candidate_window_filled: bool,
}

impl RankedSearchPage {
    /// What both surfaces say when the answer may be short of the index: a source filled its
    /// candidate window and this page ends the ranking, so matches past the window were never
    /// ranked and no further page reaches them. `None` when the ranking ends inside the window,
    /// or when a later page still holds ranked results. One sentence, so `ok search` and
    /// `search_code` cannot disagree about how complete an answer is.
    pub fn truncation_warning(&self) -> Option<String> {
        (self.candidate_window_filled && !self.has_more).then(|| {
            format!(
                "results were ranked from the top {} candidates per source; matches past them were not ranked, so narrow the query",
                self.candidate_depth
            )
        })
    }
}

/// Rank `request.query` and return the requested page.
///
/// `ok search` asks for offset 0 and prints the page. `search_code` asks for the caller's
/// offset and renders the same page with its paging metadata. The candidate depth depends on
/// where the page ends, not on its size, so `ok search --limit 170` ranks the same pool as a
/// `search_code` page at offset 150 with limit 20, and that page is a slice of the CLI's list.
pub fn ranked_search(
    repo: &Path,
    store: &dyn MetadataStore,
    request: &RankedSearchRequest<'_>,
    semantic: Option<&SemanticCandidates<'_>>,
    ranking: &RankingConfig,
) -> Result<RankedSearchPage> {
    // The check `ok search` and `search_code` both refuse a blank query with, so one message
    // and one error code (`-32602` on the wire) reach either surface.
    let query = require_search_query(request.query)?;
    let depth = candidate_depth(request.page_end());
    let (candidates, filled, merge_semantic) = match request.mode {
        SearchMode::Graph => return graph_page(repo, query, request),
        SearchMode::Code => {
            let mut candidates = lexical_candidates(repo, store, query, depth)?;
            let filled = candidates.len() >= depth;
            annotate_candidates_with_git_history(store, &mut candidates)?;
            (candidates, filled, false)
        }
        SearchMode::Hybrid => {
            let mut candidates = lexical_candidates(repo, store, query, depth)?;
            let mut filled = candidates.len() >= depth;
            annotate_candidates_with_git_history(store, &mut candidates)?;
            if let Some(semantic) = semantic.filter(|semantic| semantic.ready) {
                let semantic_candidates = (semantic.search)(query, depth)?;
                filled |= semantic_candidates.len() >= depth;
                candidates.extend(semantic_candidates);
            }
            (candidates, filled, true)
        }
        SearchMode::Semantic => {
            let semantic = semantic.ok_or_else(|| {
                OkError::Unsupported(
                    "semantic search has no semantic index to read; run `ok semantic index` first"
                        .into(),
                )
            })?;
            let candidates = (semantic.search)(query, depth)?;
            let filled = candidates.len() >= depth;
            (candidates, filled, false)
        }
    };
    Ok(rank_page(
        candidates,
        query,
        merge_semantic,
        request,
        depth,
        filled,
        ranking,
    ))
}

/// How many candidates each source is asked for, for a page ending at `page_end`: four per
/// result, at least 100 and at most [`MAX_CANDIDATE_DEPTH`]. A deeper page gets a deeper pool,
/// so a page `search_code` served from the raw index before is not cut off by a fixed pool.
pub fn candidate_depth(page_end: usize) -> usize {
    page_end.saturating_mul(4).clamp(100, MAX_CANDIDATE_DEPTH)
}

/// The candidate pool the retrieval and eval benchmarks rank, capped at 200 as their frozen
/// baselines were measured. It equals [`candidate_depth`] for a page ending at 50 or sooner.
pub fn ranking_candidate_limit(limit: usize) -> usize {
    limit.clamp(1, 100).saturating_mul(4).clamp(100, 200)
}

pub fn ranking_weights(config: &RankingConfig) -> RankingWeights {
    RankingWeights {
        text_relevance: config.text_relevance,
        exact_reference: config.exact_reference,
        graph_proximity: config.graph_proximity,
        boundary_fit: config.boundary_fit,
        runtime_corroboration: config.runtime_corroboration,
        git_cochange: config.git_cochange,
        validation_proximity: config.validation_proximity,
        memory_signal: config.memory_signal,
        path_quality: config.path_quality,
        semantic_similarity: config.semantic_similarity,
    }
}

/// The ranking every shipped search surface uses: fusion over the configured weights, with the
/// unscaled lexical score.
pub fn ranking_options(config: &RankingConfig) -> RankingOptions {
    RankingOptions {
        weights: ranking_weights(config),
        mode: RankingMode::Fusion,
        query: None,
        text_relevance_scale: TextRelevanceScale::Raw,
    }
}

/// Lexical candidates for `query`: the repository's Tantivy index when it has one, otherwise a
/// scan of the chunks the index stored.
pub fn lexical_candidates(
    repo: &Path,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let index_dir = default_index_dir(repo);
    if TantivySearchIndex::exists(&index_dir) {
        return TantivySearchIndex::open_or_create(index_dir)?.search(query, limit);
    }
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    search_chunks(&chunks, &files, &symbols, query, limit)
}

fn graph_page(
    repo: &Path,
    query: &str,
    request: &RankedSearchRequest<'_>,
) -> Result<RankedSearchPage> {
    let index_dir = default_index_dir(repo);
    if !TantivySearchIndex::exists(&index_dir) {
        return Err(OkError::Index(
            "graph search index is missing; run `ok index .` first".into(),
        ));
    }
    // Graph-node documents are not reranked, so the fetch is the page window itself.
    let window = request.window();
    let depth = window.min(MAX_CANDIDATE_DEPTH);
    let results = TantivySearchIndex::open_or_create(index_dir)?.search_graph(query, depth)?;
    let filled = window > depth && results.len() >= depth;
    Ok(page(results, request, depth, filled))
}

fn rank_page(
    candidates: Vec<SearchResult>,
    query: &str,
    merge_semantic: bool,
    request: &RankedSearchRequest<'_>,
    candidate_depth: usize,
    candidate_window_filled: bool,
    ranking: &RankingConfig,
) -> RankedSearchPage {
    let mut options = ranking_options(ranking);
    options.query = Some(query.to_string());
    let ranked = rerank_with_options(candidates, &options);
    // Deduplicating further than the page never changes the results before it. The pool bounds
    // the window, since it cannot hold more unique paths than candidates.
    let window = request.window().min(ranked.len());
    let unique = if merge_semantic {
        top_unique_paths_merging(ranked, window)
    } else {
        top_unique_paths(ranked, window)
    };
    page(unique, request, candidate_depth, candidate_window_filled)
}

fn page(
    results: Vec<SearchResult>,
    request: &RankedSearchRequest<'_>,
    candidate_depth: usize,
    candidate_window_filled: bool,
) -> RankedSearchPage {
    let has_more = request.limit.is_some() && results.len() > request.page_end();
    let results = results
        .into_iter()
        .skip(request.offset)
        .take(request.limit.unwrap_or(usize::MAX))
        .collect();
    RankedSearchPage {
        results,
        has_more,
        candidate_depth,
        candidate_window_filled,
    }
}

/// Attach bounded local git co-change evidence to `results`, and add the co-changed files the
/// query did not find as candidates of their own.
pub fn annotate_candidates_with_git_history(
    store: &dyn MetadataStore,
    results: &mut Vec<SearchResult>,
) -> Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let facts = store.analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?;
    if facts.is_empty() {
        return Ok(());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_path = files
        .into_iter()
        .map(|file| (normalize_path_fragment(&file.path.to_string_lossy()), file))
        .collect::<HashMap<_, _>>();
    // Group facts by file once; the per-result path used to rescan the full fact list.
    let mut facts_by_file: HashMap<&FileId, Vec<&AnalysisFact>> = HashMap::new();
    for fact in &facts {
        let entry = facts_by_file.entry(&fact.file_id).or_default();
        if entry.len() < 32 {
            entry.push(fact);
        }
    }
    let mut existing_paths = results
        .iter()
        .map(|result| normalize_path_fragment(&result.path.to_string_lossy()))
        .collect::<HashSet<_>>();
    let mut additions = Vec::new();
    for result in &mut *results {
        let Some(file) =
            files_by_path.get(&normalize_path_fragment(&result.path.to_string_lossy()))
        else {
            continue;
        };
        let matched = facts_by_file.get(&file.id).cloned().unwrap_or_default();
        if matched.is_empty() {
            continue;
        }
        let displayed = matched.iter().copied().take(3).collect::<Vec<_>>();
        let evidence_ids = displayed
            .iter()
            .map(|fact| fact.id.clone())
            .collect::<Vec<_>>();
        let labels = displayed
            .iter()
            .map(|fact| fact.target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        // Each line goes in beside the fact it states. Appending the lines and the ids as two
        // deduplicated lists left a ref beside a line it did not name whenever one list
        // skipped an entry the other kept (#537).
        pair_evidence_refs(result);
        for fact in &displayed {
            let evidence = format!(
                "git co-change from local history: `{}` ({})",
                fact.target, fact.message
            );
            push_evidence(result, evidence, Some(fact.id.clone()));
        }
        result.score_breakdown.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            (0.12 * matched.len() as f32).min(0.18),
            evidence_ids,
            format!("bounded local git history says this result co-changed with: {labels}"),
        ));
        for fact in matched {
            let target_path = normalize_path_fragment(&fact.target);
            if !existing_paths.insert(target_path.clone()) {
                continue;
            }
            let Some(target_file) = files_by_path.get(&target_path) else {
                continue;
            };
            let snippet = store
                .chunks_for_file(&target_file.id)?
                .first()
                .map(|chunk| chunk.text.clone())
                .unwrap_or_else(|| target_file.path.display().to_string());
            additions.push(SearchResult {
                path: target_file.path.clone(),
                line_range: None,
                snippet,
                symbol: None,
                score: 0.18 + (fact.confidence.score() * 0.05).min(0.05),
                match_reason: HISTORICAL_COCHANGE_REASON.into(),
                evidence: vec![format!(
                    "git co-change from local history: `{}` ({})",
                    fact.target, fact.message
                )],
                evidence_refs: vec![fact.id.clone()],
                confidence: fact.confidence.score(),
                score_breakdown: vec![ScoreComponent::single(
                    "similar_change_overlap",
                    0.18,
                    vec![fact.id.clone()],
                    "candidate added from bounded historical similar-change evidence",
                )],
                // Local git history is not an exact-reference source.
                exact_reference_provenance: None,
            });
        }
    }
    results.extend(additions);
    Ok(())
}

pub fn without_git_history_candidates(results: Vec<SearchResult>) -> Vec<SearchResult> {
    results
        .into_iter()
        .filter(|result| result.match_reason != HISTORICAL_COCHANGE_REASON)
        .collect()
}

/// The first result for each path, in ranked order, up to `limit` paths.
pub fn top_unique_paths(results: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if !seen.insert(path) {
            continue;
        }
        unique.push(result);
        if unique.len() == limit {
            break;
        }
    }
    unique
}

/// As [`top_unique_paths`], except that a later result for a kept path that carries semantic
/// similarity merges its evidence and score components into the kept one.
pub fn top_unique_paths_merging(results: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut indexes = HashMap::<String, usize>::new();
    let mut unique = Vec::<SearchResult>::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if let Some(index) = indexes.get(&path).copied() {
            if !has_semantic_signal(&result) {
                continue;
            }
            let existing = &mut unique[index];
            // Lines and refs merge as pairs, as they do in a context pack (#537): two hits on
            // one chunk number their lines from zero, so merged as separate lists the second
            // hit's line was kept and its colliding ref dropped.
            pair_evidence_refs(existing);
            let mut result = result;
            pair_evidence_refs(&mut result);
            merge_evidence(existing, &result);
            for component in result.score_breakdown {
                if !existing
                    .score_breakdown
                    .iter()
                    .any(|existing| existing.signal == component.signal)
                {
                    existing.score_breakdown.push(component);
                }
            }
            existing.reconcile_score_breakdown();
            continue;
        }
        if unique.len() == limit {
            continue;
        }
        indexes.insert(path, unique.len());
        unique.push(result);
    }
    unique
}

fn has_semantic_signal(result: &SearchResult) -> bool {
    result
        .score_breakdown
        .iter()
        .any(|component| component.signal == "semantic_similarity")
}

fn normalize_path_fragment(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{CodeChunk, File, IndexManifest, Language, LineRange, RepositoryId};
    use open_kioku_storage::IndexData;
    use open_kioku_storage_sqlite::SqliteStore;
    use std::path::PathBuf;

    fn lexical(path: &str, chunk: &str, score: f32) -> SearchResult {
        let evidence_id = format!("search:{path}:{chunk}");
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange { start: 1, end: 4 }),
            snippet: chunk.into(),
            symbol: None,
            score,
            match_reason: "fixture lexical match".into(),
            evidence: vec!["fixture lexical evidence".into()],
            evidence_refs: vec![evidence_id.clone()],
            confidence: 0.5,
            score_breakdown: vec![ScoreComponent::single(
                "bm25_relevance",
                score,
                vec![evidence_id],
                "fixture BM25 score",
            )],
            exact_reference_provenance: None,
        }
    }

    fn semantic(path: &str, similarity: f32) -> SearchResult {
        let evidence_id = format!("semantic:{path}:{similarity}");
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange { start: 1, end: 4 }),
            snippet: format!("semantic {path}"),
            symbol: None,
            score: similarity,
            match_reason: "fixture semantic match".into(),
            evidence: vec!["fixture semantic evidence".into()],
            evidence_refs: vec![evidence_id.clone()],
            confidence: similarity,
            score_breakdown: vec![ScoreComponent::single(
                "semantic_similarity",
                similarity,
                vec![evidence_id],
                "fixture semantic similarity",
            )],
            exact_reference_provenance: None,
        }
    }

    fn request(
        query: &str,
        mode: SearchMode,
        limit: usize,
        offset: usize,
    ) -> RankedSearchRequest<'_> {
        RankedSearchRequest {
            query,
            mode,
            limit: Some(limit),
            offset,
        }
    }

    fn paths(page: &RankedSearchPage) -> Vec<String> {
        page.results
            .iter()
            .map(|result| result.path.display().to_string())
            .collect()
    }

    /// An in-memory index of `count` Rust files, each one chunk that mentions `term`.
    fn store_with_matching_files(count: usize, term: &str) -> SqliteStore {
        store_with_matching_files_and_facts(count, term, &[])
    }

    /// As [`store_with_matching_files`], with `facts` as the index's analysis facts.
    fn store_with_matching_files_and_facts(
        count: usize,
        term: &str,
        facts: &[AnalysisFact],
    ) -> SqliteStore {
        let store = SqliteStore::open(":memory:").unwrap();
        let manifest: IndexManifest = serde_json::from_value(serde_json::json!({
            "repository": {
                "id": "repo",
                "name": "ranked-search-fixture",
                "root": ".",
                "branch": "main",
                "commit": "abc123",
                "indexed_at": "2026-01-01T00:00:00Z"
            },
            "file_count": count,
            "symbol_count": 0,
            "chunk_count": count,
            "indexed_at": "2026-01-01T00:00:00Z",
            "schema_version": 1,
            "index_mode": "full",
            "phase_reports": []
        }))
        .unwrap();
        let files = (0..count)
            .map(|index| File {
                id: FileId::new(format!("file-{index:03}")),
                repository_id: RepositoryId::new("repo"),
                path: PathBuf::from(format!("src/unit_{index:03}.rs")),
                language: Language::Rust,
                size_bytes: 64,
                content_hash: format!("hash-{index:03}"),
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
                text: format!("fn unit() {{ let _ = \"{term}\"; }}"),
                symbol_id: None,
            })
            .collect::<Vec<_>>();
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
                symbols: &[],
                chunks: &chunks,
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: facts,
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        store
    }

    /// A hit as the index and the semantic source produce it: one positional ref per line,
    /// numbered from zero under the chunk's range.
    fn chunk_hit(path: &str, lines: &[&str], signal: &str) -> SearchResult {
        let range = Some(LineRange { start: 1, end: 4 });
        let refs =
            open_kioku_core::search_result_evidence_ids(Path::new(path), &range, lines.len());
        SearchResult {
            path: PathBuf::from(path),
            line_range: range,
            snippet: String::new(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture hit".into(),
            evidence: lines.iter().map(|line| line.to_string()).collect(),
            evidence_refs: refs.clone(),
            confidence: 0.5,
            score_breakdown: vec![ScoreComponent::single(signal, 1.0, refs, "fixture")],
            exact_reference_provenance: None,
        }
    }

    fn assert_paired(result: &SearchResult) {
        assert_eq!(
            crate::evidence_pairs::pairing_violation(result),
            None,
            "{:?} / {:?}",
            result.evidence,
            result.evidence_refs
        );
    }

    #[test]
    fn a_merged_semantic_hit_keeps_each_ref_beside_its_line() {
        // Both hits name their first line `search:src/auth.rs:1-4:0`. Merged as two lists, the
        // semantic line was kept and its ref dropped as a duplicate, so the refs stopped
        // pairing with the lines (#537).
        let merged = top_unique_paths_merging(
            vec![
                chunk_hit("src/auth.rs", &["BM25 lexical match"], "bm25_relevance"),
                chunk_hit(
                    "src/auth.rs",
                    &["semantic vector similarity"],
                    "semantic_similarity",
                ),
            ],
            10,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].evidence,
            ["BM25 lexical match", "semantic vector similarity"]
        );
        assert_paired(&merged[0]);
        assert_eq!(merged[0].evidence_refs[0], "search:src/auth.rs:1-4:0");
    }

    #[test]
    fn co_change_lines_are_added_beside_the_facts_they_state() {
        let fact = |id: &str| AnalysisFact {
            id: id.into(),
            file_id: FileId::new("file-000"),
            symbol_id: None,
            target: "src/other.rs".into(),
            target_kind: open_kioku_core::GraphNodeType::File,
            edge_type: open_kioku_core::GraphEdgeType::ChangedBy,
            range: None,
            confidence: open_kioku_core::Confidence::Medium,
            source: "git-history:abc".into(),
            source_type: EvidenceSourceType::GitHistory,
            message: "co-changed in 2 commits".into(),
        };
        // Two facts that state the same line. Appended as two lists, the line was added once
        // and both ids twice, so the second id sat beside no line at all.
        let store = store_with_matching_files_and_facts(
            1,
            "ledger",
            &[fact("history-cochange:1"), fact("history-cochange:2")],
        );
        let mut results = vec![chunk_hit(
            "src/unit_000.rs",
            &["BM25 lexical match"],
            "bm25_relevance",
        )];
        annotate_candidates_with_git_history(&store, &mut results).unwrap();

        assert_paired(&results[0]);
        assert_eq!(
            results[0].evidence_refs,
            ["search:src/unit_000.rs:1-4:0", "history-cochange:1"]
        );
        let history = results[0]
            .score_breakdown
            .iter()
            .find(|component| component.signal == "similar_change_overlap")
            .expect("the co-change adjustment");
        assert_eq!(
            history.evidence_ids,
            ["history-cochange:1", "history-cochange:2"]
        );
    }

    #[test]
    fn the_page_is_the_reranked_order_with_one_result_per_path() {
        // The index lists the file that only mentions the term first. The file the query names
        // outranks it once ranked, which is the order `ok search` prints and `search_code` used
        // to skip.
        let index_order = vec![
            lexical("src/notes.rs", "notes-first-chunk", 9.0),
            lexical("src/notes.rs", "notes-second-chunk", 8.0),
            lexical("src/ledger.rs", "ledger-chunk", 2.0),
            lexical("src/archive.rs", "archive-chunk", 1.0),
        ];
        let page = rank_page(
            index_order,
            "ledger",
            false,
            &request("ledger", SearchMode::Code, 10, 0),
            100,
            false,
            &RankingConfig::default(),
        );
        assert_eq!(
            paths(&page),
            ["src/ledger.rs", "src/notes.rs", "src/archive.rs"]
        );
        assert_eq!(page.results[1].snippet, "notes-first-chunk");
        assert!(!page.has_more);
        assert!(page.results.iter().all(|result| result
            .score_breakdown
            .iter()
            .any(|component| component.signal == "text_relevance")));
    }

    #[test]
    fn pages_are_slices_of_one_ranking() {
        let candidates = || {
            (0..5)
                .map(|index| {
                    lexical(
                        &format!("src/widget_{index}.rs"),
                        "chunk",
                        10.0 - index as f32,
                    )
                })
                .collect::<Vec<_>>()
        };
        let config = RankingConfig::default();
        let full = rank_page(
            candidates(),
            "widget",
            false,
            &request("widget", SearchMode::Code, 5, 0),
            100,
            false,
            &config,
        );
        assert_eq!(full.results.len(), 5);
        assert!(!full.has_more);

        let mut paged = Vec::new();
        for (offset, has_more) in [(0, true), (2, true), (4, false)] {
            let page = rank_page(
                candidates(),
                "widget",
                false,
                &request("widget", SearchMode::Code, 2, offset),
                100,
                false,
                &config,
            );
            assert_eq!(page.has_more, has_more, "offset {offset}");
            paged.extend(paths(&page));
        }
        assert_eq!(paged, paths(&full));
    }

    #[test]
    fn no_limit_returns_every_unique_path_in_the_pool() {
        // `ok search --limit 0` has always printed every unique path; hybrid merging keeps it.
        let candidates = || {
            vec![
                lexical("src/alpha.rs", "alpha-first", 9.0),
                lexical("src/alpha.rs", "alpha-second", 8.0),
                lexical("src/beta.rs", "beta", 7.0),
                lexical("src/gamma.rs", "gamma", 6.0),
            ]
        };
        let every = RankedSearchRequest {
            query: "widget",
            mode: SearchMode::Code,
            limit: None,
            offset: 0,
        };
        for merge_semantic in [false, true] {
            let page = rank_page(
                candidates(),
                "widget",
                merge_semantic,
                &every,
                100,
                false,
                &RankingConfig::default(),
            );
            assert_eq!(
                paths(&page),
                ["src/alpha.rs", "src/beta.rs", "src/gamma.rs"],
                "merge_semantic {merge_semantic}"
            );
            assert!(!page.has_more);
        }
    }

    #[test]
    fn the_candidate_depth_grows_with_the_page_end_up_to_the_cap() {
        // The benchmarks' pool is unchanged for every page they rank.
        for page_end in 0..=50 {
            assert_eq!(
                candidate_depth(page_end),
                ranking_candidate_limit(page_end),
                "page end {page_end}"
            );
        }
        assert_eq!(candidate_depth(60), 240);
        assert_eq!(candidate_depth(170), MAX_CANDIDATE_DEPTH);
        assert_eq!(candidate_depth(usize::MAX), MAX_CANDIDATE_DEPTH);
        // Never shallower than the `offset + limit + 1` raw candidates `search_code` fetched.
        for page_end in 0..MAX_CANDIDATE_DEPTH {
            assert!(candidate_depth(page_end) > page_end, "page end {page_end}");
        }
    }

    #[test]
    fn a_deep_page_is_ranked_from_a_pool_that_reaches_it() {
        // 300 matching files. `search_code` served pages up to 500 raw candidates deep, so every
        // page that reaches into the matches must still come back with results.
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_matching_files(300, "ledgerline");
        let config = RankingConfig::default();

        let deep = ranked_search(
            temp.path(),
            &store,
            &request("ledgerline", SearchMode::Code, 20, 150),
            None,
            &config,
        )
        .unwrap();
        assert_eq!(deep.results.len(), 20);
        assert!(deep.has_more);
        assert_eq!(deep.candidate_depth, MAX_CANDIDATE_DEPTH);
        assert!(!deep.candidate_window_filled);

        // The list `ok search --limit 170` prints holds that page at 150..170.
        let whole = ranked_search(
            temp.path(),
            &store,
            &request("ledgerline", SearchMode::Code, 170, 0),
            None,
            &config,
        )
        .unwrap();
        assert_eq!(whole.results.len(), 170);
        assert_eq!(paths(&deep), paths(&whole)[150..170].to_vec());

        // Past the 200 candidates the benchmarks' fixed pool holds, which would return nothing.
        let past_a_fixed_pool = ranked_search(
            temp.path(),
            &store,
            &request("ledgerline", SearchMode::Code, 20, 250),
            None,
            &config,
        )
        .unwrap();
        assert_eq!(past_a_fixed_pool.results.len(), 20);
        assert!(past_a_fixed_pool.has_more);

        let past_the_matches = ranked_search(
            temp.path(),
            &store,
            &request("ledgerline", SearchMode::Code, 20, 300),
            None,
            &config,
        )
        .unwrap();
        assert!(past_the_matches.results.is_empty());
        assert!(!past_the_matches.has_more);
    }

    #[test]
    fn a_result_keeps_its_typed_exact_reference_provenance() {
        // Ranking reads `exact_reference_provenance`, not prose, so the page has to carry it
        // out unchanged. Deduplication drops a duplicate whole; it never moves one record's
        // provenance onto another, which would claim an exact reference that range lacks.
        let mut exact = lexical("src/exact.rs", "exact-chunk", 5.0);
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        let candidates = vec![
            exact,
            lexical("src/exact.rs", "exact-second-chunk", 4.0),
            lexical("src/other.rs", "other", 9.0),
        ];
        let page = rank_page(
            candidates,
            "exact",
            false,
            &request("exact", SearchMode::Code, 10, 0),
            100,
            false,
            &RankingConfig::default(),
        );
        let by_path = |path: &str| {
            page.results
                .iter()
                .find(|result| result.path.ends_with(path))
                .unwrap_or_else(|| panic!("{path} should be on the page"))
        };
        assert_eq!(
            by_path("exact.rs").exact_reference_provenance,
            Some(EvidenceSourceType::Scip)
        );
        assert!(by_path("exact.rs").is_exact_reference());
        assert_eq!(by_path("other.rs").exact_reference_provenance, None);
    }

    #[test]
    fn a_filled_candidate_window_warns_only_when_the_page_ends_the_ranking() {
        let page = |has_more: bool, candidate_window_filled: bool| RankedSearchPage {
            results: Vec::new(),
            has_more,
            candidate_depth: 500,
            candidate_window_filled,
        };
        let warning = page(false, true)
            .truncation_warning()
            .expect("a filled window that ends the ranking is reported");
        assert!(warning.contains("top 500 candidates"), "{warning}");
        // A later page still holds ranked results, so nothing is missing yet.
        assert!(page(true, true).truncation_warning().is_none());
        // The ranking ended inside the window: the answer is complete.
        assert!(page(false, false).truncation_warning().is_none());
    }

    #[test]
    fn the_configured_ranking_weights_decide_the_order() {
        let candidates = || {
            vec![
                lexical("src/alpha.rs", "alpha", 9.0),
                lexical("tests/beta.rs", "beta", 1.0),
            ]
        };
        let search = request("reconcile", SearchMode::Code, 10, 0);
        let by_default = rank_page(
            candidates(),
            "reconcile",
            false,
            &search,
            100,
            false,
            &RankingConfig::default(),
        );
        assert_eq!(paths(&by_default), ["src/alpha.rs", "tests/beta.rs"]);

        let configured = RankingConfig {
            text_relevance: 0.01,
            validation_proximity: 100.0,
            ..RankingConfig::default()
        };
        let reordered = rank_page(
            candidates(),
            "reconcile",
            false,
            &search,
            100,
            false,
            &configured,
        );
        assert_eq!(paths(&reordered), ["tests/beta.rs", "src/alpha.rs"]);
    }

    #[test]
    fn semantic_candidates_are_ranked_and_an_unready_index_stays_out_of_hybrid() {
        // No `.ok` under this directory, so the lexical source is a scan of an empty store.
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(":memory:").unwrap();
        let config = RankingConfig::default();

        let answers = |_query: &str, _depth: usize| -> Result<Vec<SearchResult>> {
            Ok(vec![
                semantic("src/alpha.rs", 0.2),
                semantic("src/zeta.rs", 0.9),
                semantic("src/zeta.rs", 0.4),
            ])
        };
        let ready = SemanticCandidates {
            ready: true,
            search: &answers,
        };
        let page = ranked_search(
            temp.path(),
            &store,
            &request("session token", SearchMode::Semantic, 10, 0),
            Some(&ready),
            &config,
        )
        .unwrap();
        // Similarity, not the source's order or the path, decides.
        assert_eq!(paths(&page), ["src/zeta.rs", "src/alpha.rs"]);

        let refuses = |_query: &str, _depth: usize| -> Result<Vec<SearchResult>> {
            Err(OkError::Unsupported("semantic index is missing".into()))
        };
        let not_ready = SemanticCandidates {
            ready: false,
            search: &refuses,
        };
        let hybrid = ranked_search(
            temp.path(),
            &store,
            &request("session token", SearchMode::Hybrid, 10, 0),
            Some(&not_ready),
            &config,
        )
        .unwrap();
        assert!(hybrid.results.is_empty());

        let semantic_error = ranked_search(
            temp.path(),
            &store,
            &request("session token", SearchMode::Semantic, 10, 0),
            Some(&not_ready),
            &config,
        )
        .unwrap_err();
        assert!(
            matches!(semantic_error, OkError::Unsupported(_)),
            "{semantic_error}"
        );

        let graph_error = ranked_search(
            temp.path(),
            &store,
            &request("session token", SearchMode::Graph, 10, 0),
            None,
            &config,
        )
        .unwrap_err();
        assert!(matches!(graph_error, OkError::Index(_)), "{graph_error}");

        let blank = ranked_search(
            temp.path(),
            &store,
            &request("   ", SearchMode::Code, 10, 0),
            None,
            &config,
        )
        .unwrap_err();
        assert!(blank.is_invalid_input(), "{blank}");
        assert!(
            blank
                .to_string()
                .contains(open_kioku_storage::BLANK_SEARCH_QUERY_MESSAGE),
            "{blank}"
        );
    }
}
