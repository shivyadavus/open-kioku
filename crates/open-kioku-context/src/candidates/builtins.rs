use super::{CandidateRequest, CandidateStream, StreamCandidate};
use crate::{search_candidates, TaskSearchIntent};
use open_kioku_core::{
    identity::symbol_node_id, AnalysisFact, CodeChunk, EvidenceSourceType, File, GraphEdge, NodeId,
    RetrievalAuthority, RetrievalSourceKind, SearchResult, Symbol, TestTarget,
};
use open_kioku_ranking::rerank_baseline;
use open_kioku_storage::{HistoryStore, OkStore};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct BuiltinCandidateContext<'a> {
    pub store: &'a dyn OkStore,
    pub history_store: Option<&'a dyn HistoryStore>,
    pub files: &'a [File],
    pub chunks: &'a [CodeChunk],
    pub symbols: &'a [Symbol],
}

impl<'a> BuiltinCandidateContext<'a> {
    pub fn collect(&self, request: &CandidateRequest) -> Vec<CandidateStream> {
        self.collect_excluding(request, &BTreeSet::new())
    }

    pub fn collect_excluding(
        &self,
        request: &CandidateRequest,
        excluded: &BTreeSet<RetrievalSourceKind>,
    ) -> Vec<CandidateStream> {
        // Exact anchors are also the evidence-backed seeds for graph/history retrieval, so they
        // are computed even when an external ExactSemantic source owns the emitted stream.
        let exact = self.exact_symbol_stream(request);
        let anchor_symbols = exact
            .candidates
            .iter()
            .filter(|candidate| candidate.authority == RetrievalAuthority::Exact)
            .filter_map(|candidate| candidate.result.symbol.clone())
            .collect::<Vec<_>>();
        let mut streams = Vec::new();
        if !excluded.contains(&RetrievalSourceKind::Lexical) {
            streams.push(self.lexical_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::ExactSemantic) {
            streams.push(exact);
        }
        if !excluded.contains(&RetrievalSourceKind::Graph) {
            streams.push(self.graph_stream(request, &anchor_symbols));
        }
        if !excluded.contains(&RetrievalSourceKind::Validation) {
            streams.push(self.validation_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::Runtime) {
            streams.push(self.runtime_stream(request));
        }
        if !excluded.contains(&RetrievalSourceKind::GitHistory) {
            streams.push(self.history_stream(request, &anchor_symbols));
        }
        streams
    }

    fn lexical_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let intent = TaskSearchIntent::parse(&request.task);
        match search_candidates(
            self.chunks,
            self.files,
            self.symbols,
            &request.task,
            request.limit,
            &intent,
        ) {
            Ok(results) => CandidateStream::success(
                RetrievalSourceKind::Lexical,
                rerank_baseline(results)
                    .into_iter()
                    .map(|result| {
                        StreamCandidate::from_result(
                            result,
                            RetrievalAuthority::Heuristic,
                            "lexical repository search candidate",
                        )
                    })
                    .collect(),
            ),
            Err(err) => CandidateStream::unavailable(
                RetrievalSourceKind::Lexical,
                format!("lexical candidate stream unavailable: {err}"),
            ),
        }
    }

    fn exact_symbol_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let keys = symbol_anchor_keys(&request.task, &request.search_terms);
        if keys.is_empty() {
            return CandidateStream::success(RetrievalSourceKind::ExactSemantic, Vec::new());
        }

        let match_counts = symbol_anchor_match_counts(&keys, self.symbols);

        let mut caveats = match_counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(anchor, count)| {
                format!(
                    "exact symbol anchor `{anchor}` is ambiguous across {count} symbols; candidates are retained as corroborating possibilities and cannot seed graph/history expansion"
                )
            })
            .collect::<Vec<_>>();
        caveats.sort();

        let mut candidates = self
            .symbols
            .iter()
            .filter_map(|symbol| {
                let (matched, authority) = select_symbol_anchor(symbol, &keys, &match_counts)?;
                let file = self.files.iter().find(|file| file.id == symbol.file_id)?;
                let result = result_for_file(
                    file,
                    Some(symbol.clone()),
                    self.chunks,
                    1.0,
                    format!("exact semantic symbol anchor `{matched}`"),
                    vec![format!("symbol:{}", symbol.id.0)],
                    if authority == RetrievalAuthority::Exact {
                        1.0
                    } else {
                        0.8
                    },
                );
                Some(StreamCandidate::from_result(
                    result,
                    authority,
                    if authority == RetrievalAuthority::Exact {
                        format!(
                            "query anchor uniquely resolves to `{}`",
                            symbol.qualified_name
                        )
                    } else {
                        format!(
                            "query anchor exactly matches `{}` but remains ambiguous",
                            symbol.qualified_name
                        )
                    },
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .authority
                .cmp(&left.authority)
                .then_with(|| left.result.path.cmp(&right.result.path))
                .then_with(|| {
                    left.result
                        .symbol
                        .as_ref()
                        .map(|symbol| &symbol.qualified_name)
                        .cmp(
                            &right
                                .result
                                .symbol
                                .as_ref()
                                .map(|symbol| &symbol.qualified_name),
                        )
                })
        });
        candidates.truncate(request.limit);
        CandidateStream {
            source: RetrievalSourceKind::ExactSemantic,
            candidates,
            caveats,
            available: true,
        }
    }

    fn graph_stream(
        &self,
        request: &CandidateRequest,
        anchor_symbols: &[Symbol],
    ) -> CandidateStream {
        if anchor_symbols.is_empty() {
            return CandidateStream::success(RetrievalSourceKind::Graph, Vec::new());
        }
        let files_by_id = self
            .files
            .iter()
            .map(|file| (file.id.clone(), file))
            .collect::<BTreeMap<_, _>>();
        let symbols_by_id = self
            .symbols
            .iter()
            .map(|symbol| (symbol.id.clone(), symbol))
            .collect::<BTreeMap<_, _>>();
        let mut by_path = BTreeMap::<String, StreamCandidate>::new();
        let mut caveats = Vec::new();

        for anchor in anchor_symbols.iter().take(8) {
            let node_id = symbol_node_id(anchor);
            match self.store.neighbors(&node_id.0, request.limit.min(50)) {
                Ok((nodes, edges)) => {
                    let mut omitted_without_direct_evidence = 0usize;
                    for node in nodes {
                        if node.id == node_id {
                            continue;
                        }
                        let edge_ids = incident_edge_ids(&node_id, &node.id, &edges);
                        if edge_ids.is_empty() {
                            omitted_without_direct_evidence += 1;
                            continue;
                        }
                        let file = node
                            .file_id
                            .as_ref()
                            .and_then(|file_id| files_by_id.get(file_id).copied())
                            .or_else(|| {
                                node.symbol_id
                                    .as_ref()
                                    .and_then(|symbol_id| symbols_by_id.get(symbol_id).copied())
                                    .and_then(|symbol| files_by_id.get(&symbol.file_id).copied())
                            });
                        let Some(file) = file else {
                            continue;
                        };
                        let symbol = node
                            .symbol_id
                            .as_ref()
                            .and_then(|symbol_id| symbols_by_id.get(symbol_id).copied())
                            .cloned();
                        let result = result_for_file(
                            file,
                            symbol,
                            self.chunks,
                            0.85,
                            format!("graph neighbor of `{}`", anchor.qualified_name),
                            edge_ids.clone(),
                            0.9,
                        );
                        let key = normalized_path(&file.path);
                        let candidate = StreamCandidate::from_result(
                            result,
                            RetrievalAuthority::Corroborating,
                            "evidence-graph neighbor backed by a direct edge from an exact symbol",
                        );
                        if let Some(existing) = by_path.get_mut(&key) {
                            for evidence in candidate.result.evidence {
                                if !existing.result.evidence.contains(&evidence) {
                                    existing.result.evidence.push(evidence);
                                }
                            }
                            for evidence_ref in candidate.evidence_refs {
                                if !existing.evidence_refs.contains(&evidence_ref) {
                                    existing.evidence_refs.push(evidence_ref.clone());
                                }
                                if !existing.result.evidence_refs.contains(&evidence_ref) {
                                    existing.result.evidence_refs.push(evidence_ref);
                                }
                            }
                            existing.evidence_refs.sort();
                            existing.evidence_refs.dedup();
                            existing.result.evidence_refs.sort();
                            existing.result.evidence_refs.dedup();
                        } else {
                            by_path.insert(key, candidate);
                        }
                    }
                    if omitted_without_direct_evidence > 0 {
                        caveats.push(format!(
                            "graph stream omitted {omitted_without_direct_evidence} neighbor(s) for `{}` because no direct edge evidence connected them to the anchor",
                            anchor.qualified_name
                        ));
                    }
                }
                Err(err) => caveats.push(format!(
                    "graph stream failed for `{}`: {err}",
                    anchor.qualified_name
                )),
            }
        }
        let mut candidates = by_path.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.result.path.cmp(&right.result.path));
        candidates.truncate(request.limit);
        CandidateStream {
            source: RetrievalSourceKind::Graph,
            candidates,
            caveats,
            available: true,
        }
    }

    fn validation_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let terms = retrieval_terms(request);
        let tests = match self.store.tests() {
            Ok(tests) => tests,
            Err(err) => {
                return CandidateStream::unavailable(
                    RetrievalSourceKind::Validation,
                    format!("validation candidate stream unavailable: {err}"),
                )
            }
        };
        let files_by_id = self
            .files
            .iter()
            .map(|file| (file.id.clone(), file))
            .collect::<BTreeMap<_, _>>();
        let mut scored = tests
            .into_iter()
            .filter_map(|test| {
                let haystack = format!(
                    "{} {} {}",
                    test.name,
                    test.command.as_deref().unwrap_or(""),
                    test.id
                )
                .to_ascii_lowercase();
                let overlap = term_overlap(&terms, &haystack);
                if overlap == 0 {
                    return None;
                }
                let file = files_by_id.get(&test.file_id).copied()?;
                let result = result_for_test(file, &test, overlap as f32);
                Some((
                    overlap,
                    StreamCandidate::from_result(
                        result,
                        RetrievalAuthority::Corroborating,
                        "validation/test target overlaps the task vocabulary",
                    ),
                ))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.result.path.cmp(&right.1.result.path))
        });
        CandidateStream::success(
            RetrievalSourceKind::Validation,
            scored
                .into_iter()
                .map(|(_, candidate)| candidate)
                .take(request.limit)
                .collect(),
        )
    }

    fn runtime_stream(&self, request: &CandidateRequest) -> CandidateStream {
        let terms = retrieval_terms(request);
        let facts = match self.store.analysis_facts(
            Some(EvidenceSourceType::Runtime),
            request.limit.saturating_mul(20).clamp(100, 5_000),
        ) {
            Ok(facts) => facts,
            Err(err) => {
                return CandidateStream::unavailable(
                    RetrievalSourceKind::Runtime,
                    format!("runtime candidate stream unavailable: {err}"),
                )
            }
        };
        let files_by_id = self
            .files
            .iter()
            .map(|file| (file.id.clone(), file))
            .collect::<BTreeMap<_, _>>();
        let mut scored = facts
            .into_iter()
            .filter(|fact| fact.source_type == EvidenceSourceType::Runtime)
            .filter_map(|fact| {
                let haystack = format!("{} {} {}", fact.target, fact.message, fact.source)
                    .to_ascii_lowercase();
                let overlap = term_overlap(&terms, &haystack);
                if overlap == 0 {
                    return None;
                }
                let file = files_by_id.get(&fact.file_id).copied()?;
                let result = result_for_runtime_fact(file, &fact, self.chunks, overlap as f32);
                Some((
                    overlap,
                    StreamCandidate::from_result(
                        result,
                        RetrievalAuthority::Corroborating,
                        "runtime evidence overlaps the requested workflow",
                    ),
                ))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.result.path.cmp(&right.1.result.path))
        });
        CandidateStream::success(
            RetrievalSourceKind::Runtime,
            scored
                .into_iter()
                .map(|(_, candidate)| candidate)
                .take(request.limit)
                .collect(),
        )
    }

    fn history_stream(
        &self,
        request: &CandidateRequest,
        anchor_symbols: &[Symbol],
    ) -> CandidateStream {
        let Some(history_store) = self.history_store else {
            return CandidateStream::unavailable(
                RetrievalSourceKind::GitHistory,
                "git-history candidate stream is not configured",
            );
        };
        let query = open_kioku_core::SimilarChangeQuery {
            task: Some(request.task.clone()),
            paths: Vec::new(),
            symbols: anchor_symbols
                .iter()
                .map(|symbol| symbol.id.0.clone())
                .collect(),
        };
        let report = match history_store.similar_changes(&query, request.limit.min(20)) {
            Ok(report) => report,
            Err(err) => {
                return CandidateStream::unavailable(
                    RetrievalSourceKind::GitHistory,
                    format!("git-history candidate stream unavailable: {err}"),
                )
            }
        };
        let files_by_path = self
            .files
            .iter()
            .map(|file| (normalized_path(&file.path), file))
            .collect::<BTreeMap<_, _>>();
        let mut by_path = BTreeMap::<String, StreamCandidate>::new();
        for hit in report.hits {
            for changed_file in &hit.change.touched_paths {
                let key = normalized_path(changed_file);
                let Some(file) = files_by_path.get(&key).copied() else {
                    continue;
                };
                let result = result_for_file(
                    file,
                    None,
                    self.chunks,
                    hit.score,
                    format!(
                        "historically similar change `{}`",
                        hit.change.commit.summary
                    ),
                    vec![format!("history:similar-change:{}", hit.change.commit.id.0)],
                    0.7,
                );
                by_path.entry(key).or_insert_with(|| {
                    StreamCandidate::from_result(
                        result,
                        RetrievalAuthority::Heuristic,
                        "file appeared in a similar historical change",
                    )
                });
            }
        }
        let mut candidates = by_path.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .raw_score
                .unwrap_or_default()
                .total_cmp(&left.raw_score.unwrap_or_default())
                .then_with(|| left.result.path.cmp(&right.result.path))
        });
        CandidateStream::success(
            RetrievalSourceKind::GitHistory,
            candidates.into_iter().take(request.limit).collect(),
        )
    }
}

pub(super) fn incident_edge_ids(
    anchor: &NodeId,
    candidate: &NodeId,
    edges: &[GraphEdge],
) -> Vec<String> {
    let mut ids = edges
        .iter()
        .filter(|edge| {
            (&edge.from == anchor && &edge.to == candidate)
                || (&edge.from == candidate && &edge.to == anchor)
        })
        .map(|edge| edge.id.0.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn result_for_test(file: &File, test: &TestTarget, score: f32) -> SearchResult {
    SearchResult {
        path: file.path.clone(),
        line_range: test.range.clone(),
        snippet: test.name.clone(),
        symbol: None,
        score,
        match_reason: format!("validation target `{}`", test.name),
        evidence: vec![format!("validation target `{}`", test.name)],
        evidence_refs: vec![format!("test:{}", test.id)],
        confidence: test.confidence.score(),
        score_breakdown: Vec::new(),
    }
}

fn result_for_runtime_fact(
    file: &File,
    fact: &AnalysisFact,
    chunks: &[CodeChunk],
    score: f32,
) -> SearchResult {
    result_for_file(
        file,
        None,
        chunks,
        score,
        format!("runtime evidence: {}", fact.message),
        vec![fact.id.clone()],
        fact.confidence.score(),
    )
}

fn result_for_file(
    file: &File,
    symbol: Option<Symbol>,
    chunks: &[CodeChunk],
    score: f32,
    reason: String,
    evidence_refs: Vec<String>,
    confidence: f32,
) -> SearchResult {
    let symbol_id = symbol.as_ref().map(|symbol| &symbol.id);
    let chunk = chunks
        .iter()
        .find(|chunk| chunk.file_id == file.id && chunk.symbol_id.as_ref() == symbol_id)
        .or_else(|| chunks.iter().find(|chunk| chunk.file_id == file.id));
    SearchResult {
        path: file.path.clone(),
        line_range: symbol
            .as_ref()
            .and_then(|symbol| symbol.range.clone())
            .or_else(|| chunk.map(|chunk| chunk.range.clone())),
        snippet: chunk.map(|chunk| chunk.text.clone()).unwrap_or_default(),
        symbol,
        score,
        match_reason: reason.clone(),
        evidence: vec![reason],
        evidence_refs,
        confidence,
        score_breakdown: Vec::new(),
    }
}

fn symbol_anchor_match_counts(
    keys: &BTreeSet<String>,
    symbols: &[Symbol],
) -> BTreeMap<String, usize> {
    keys.iter()
        .filter_map(|key| {
            let count = symbols
                .iter()
                .filter(|symbol| symbol_matches_anchor(symbol, key))
                .count();
            (count > 0).then(|| (key.clone(), count))
        })
        .collect()
}

fn symbol_matches_anchor(symbol: &Symbol, key: &str) -> bool {
    exact_symbol_key(&symbol.name) == key || exact_symbol_key(&symbol.qualified_name) == key
}

fn select_symbol_anchor(
    symbol: &Symbol,
    keys: &BTreeSet<String>,
    match_counts: &BTreeMap<String, usize>,
) -> Option<(String, RetrievalAuthority)> {
    select_symbol_anchor_names(&symbol.name, &symbol.qualified_name, keys, match_counts)
}

fn select_symbol_anchor_names(
    name: &str,
    qualified_name: &str,
    keys: &BTreeSet<String>,
    match_counts: &BTreeMap<String, usize>,
) -> Option<(String, RetrievalAuthority)> {
    let qualified = exact_symbol_key(qualified_name);
    let name = exact_symbol_key(name);
    let mut matched = Vec::new();
    if keys.contains(&qualified) {
        matched.push(qualified);
    }
    if keys.contains(&name) && !matched.iter().any(|value| value == &name) {
        matched.push(name);
    }
    if matched.is_empty() {
        return None;
    }

    // A unique qualified or bare anchor is authoritative. If every matching anchor is ambiguous,
    // keep the candidate as a possibility but never let it seed graph/history expansion.
    if let Some(anchor) = matched
        .iter()
        .find(|anchor| match_counts.get(*anchor).copied() == Some(1))
    {
        return Some((anchor.clone(), RetrievalAuthority::Exact));
    }
    Some((matched[0].clone(), RetrievalAuthority::Corroborating))
}

fn symbol_anchor_keys(task: &str, expanded_terms: &[String]) -> BTreeSet<String> {
    let mut anchors = BTreeSet::new();

    // Expanded terms are useful only when they still look like source identifiers. Ordinary
    // natural-language retrieval terms must remain heuristic and cannot manufacture exact truth.
    for term in expanded_terms {
        if is_explicit_symbol_anchor(term) {
            anchors.insert(exact_symbol_key(term));
        }
    }

    for token in task.split_whitespace() {
        let token = trim_symbol_punctuation(token);
        if is_explicit_symbol_anchor(token) {
            anchors.insert(exact_symbol_key(token));
        }
    }

    // Backticks explicitly mark code/symbol text, including lower-case Python/Go/Rust names.
    for (index, segment) in task.split('`').enumerate() {
        if index % 2 == 1 {
            let segment = segment.trim();
            if is_symbol_expression(segment) {
                anchors.insert(exact_symbol_key(segment));
            }
        }
    }

    // Also accept lower-case names when the task explicitly labels them as a code entity.
    let words = task.split_whitespace().collect::<Vec<_>>();
    for pair in words.windows(2) {
        let introducer = pair[0]
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
            .to_ascii_lowercase();
        if matches!(
            introducer.as_str(),
            "fn" | "function"
                | "method"
                | "class"
                | "struct"
                | "trait"
                | "interface"
                | "type"
                | "symbol"
        ) {
            let candidate = trim_symbol_punctuation(pair[1]);
            if is_symbol_expression(candidate) {
                anchors.insert(exact_symbol_key(candidate));
            }
        }
    }

    anchors.retain(|anchor| anchor.len() >= 3);
    anchors
}

fn exact_symbol_key(value: &str) -> String {
    // Authoritative matching is case-sensitive for all Tier-1 languages and preserves namespace
    // boundaries. Dot and colon namespace syntax are canonicalized to `::`, but unlike lexical
    // normalization we never collapse `a::bc` into the same key as `ab::c`.
    let value = trim_symbol_punctuation(value);
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if (ch == ':' || ch == '.') && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts.join("::")
}

fn trim_symbol_punctuation(value: &str) -> &str {
    value.trim_matches(|ch: char| {
        !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' || ch == '.')
    })
}

fn is_explicit_symbol_anchor(value: &str) -> bool {
    let value = trim_symbol_punctuation(value);
    if !is_symbol_expression(value) {
        return false;
    }
    let has_lower = value.chars().any(|ch| ch.is_ascii_lowercase());
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    (has_lower && has_upper)
        || value.contains('_')
        || value.contains("::")
        || (value.contains('.') && !looks_like_source_path(value))
}

fn is_symbol_expression(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' || ch == '.')
        && value.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn looks_like_source_path(value: &str) -> bool {
    [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".java", ".py", ".go", ".md", ".json",
    ]
    .iter()
    .any(|suffix| value.to_ascii_lowercase().ends_with(suffix))
}

fn retrieval_terms(request: &CandidateRequest) -> Vec<String> {
    request
        .search_terms
        .iter()
        .flat_map(|term| term.split_whitespace())
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn term_overlap(terms: &[String], haystack: &str) -> usize {
    terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count()
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

#[cfg(test)]
mod exact_authority_tests {
    use super::*;

    #[test]
    fn mixed_qualified_and_bare_anchor_keeps_unrelated_same_name_ambiguous() {
        let keys = BTreeSet::from(["Repository".to_string(), "foo::Repository".to_string()]);
        let counts = BTreeMap::from([
            ("Repository".to_string(), 2usize),
            ("foo::Repository".to_string(), 1usize),
        ]);

        assert_eq!(
            select_symbol_anchor_names("Repository", "foo.Repository", &keys, &counts),
            Some(("foo::Repository".into(), RetrievalAuthority::Exact))
        );
        assert_eq!(
            select_symbol_anchor_names("Repository", "bar.Repository", &keys, &counts),
            Some(("Repository".into(), RetrievalAuthority::Corroborating))
        );
    }

    #[test]
    fn authoritative_keys_preserve_case_and_namespace_boundaries() {
        assert_eq!(exact_symbol_key("a::bc"), "a::bc");
        assert_eq!(exact_symbol_key("ab::c"), "ab::c");
        assert_ne!(exact_symbol_key("a::bc"), exact_symbol_key("ab::c"));
        assert_eq!(exact_symbol_key("foo.Repository"), "foo::Repository");
        assert_ne!(
            exact_symbol_key("PaymentService"),
            exact_symbol_key("paymentservice")
        );
    }

    #[test]
    fn case_or_namespace_near_matches_cannot_receive_exact_authority() {
        let keys = BTreeSet::from(["a::bc".to_string(), "PaymentService".to_string()]);
        let counts = BTreeMap::from([
            ("a::bc".to_string(), 1usize),
            ("PaymentService".to_string(), 1usize),
        ]);
        assert_eq!(
            select_symbol_anchor_names("c", "ab::c", &keys, &counts),
            None
        );
        assert_eq!(
            select_symbol_anchor_names("paymentservice", "src::paymentservice", &keys, &counts),
            None
        );
    }

    #[test]
    fn plain_natural_language_terms_do_not_create_exact_symbol_anchors() {
        let expanded = vec![
            "add".into(),
            "caching".into(),
            "repository".into(),
            "request".into(),
        ];
        let anchors = symbol_anchor_keys("add caching to repository request", &expanded);
        assert!(anchors.is_empty());
    }

    #[test]
    fn explicit_code_shapes_and_labels_remain_exact_anchor_candidates() {
        let anchors = symbol_anchor_keys(
            "change PaymentService and method process like `load` via verify_change",
            &[],
        );
        assert!(anchors.contains("PaymentService"));
        assert!(anchors.contains("process"));
        assert!(anchors.contains("load"));
        assert!(anchors.contains("verify_change"));
    }

    #[test]
    fn source_paths_are_not_promoted_to_exact_symbol_anchors() {
        let anchors = symbol_anchor_keys("change src/Foo.java and config.json", &[]);
        assert!(!anchors.contains("src::Foo::java"));
        assert!(!anchors.contains("config::json"));
    }
}
