use open_kioku_core::{
    RetrievalAuthority, RetrievalContribution, RetrievalDiagnostics, RetrievalSourceKind,
    RetrievalTrace, ScoreComponent, SearchResult,
};
use open_kioku_errors::Result;
use open_kioku_ranking::{RankingMode, RankingOptions, RankingSignal, RankingWeights};
use open_kioku_storage::SearchIndex;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub mod builtins;

pub const DEFAULT_RRF_K: f32 = 60.0;

#[derive(Debug, Clone)]
pub struct CandidateRequest {
    pub task: String,
    pub search_terms: Vec<String>,
    pub limit: usize,
}

impl CandidateRequest {
    pub fn new(task: impl Into<String>, search_terms: Vec<String>, limit: usize) -> Self {
        Self {
            task: task.into(),
            search_terms,
            limit: limit.clamp(1, 200),
        }
    }
}

pub trait ContextCandidateSource: Send + Sync {
    fn source(&self) -> RetrievalSourceKind;
    fn retrieve(&self, request: &CandidateRequest) -> Result<CandidateStream>;
}

pub struct SearchIndexCandidateSource<T: SearchIndex> {
    index: T,
}

impl<T: SearchIndex> SearchIndexCandidateSource<T> {
    pub fn new(index: T) -> Self {
        Self { index }
    }
}

impl<T: SearchIndex> ContextCandidateSource for SearchIndexCandidateSource<T> {
    fn source(&self) -> RetrievalSourceKind {
        RetrievalSourceKind::Lexical
    }

    fn retrieve(&self, request: &CandidateRequest) -> Result<CandidateStream> {
        let terms = if request.search_terms.is_empty() {
            vec![request.task.as_str()]
        } else {
            request.search_terms.iter().map(String::as_str).collect()
        };
        let mut by_path = BTreeMap::<String, (usize, SearchResult)>::new();
        for term in terms {
            for (index, mut result) in self
                .index
                .search(term, request.limit)?
                .into_iter()
                .filter(|result| !is_document_candidate_path(&result.path.to_string_lossy()))
                .enumerate()
            {
                let rank = index + 1;
                if term != request.task {
                    let evidence = format!("expanded task query `{term}` matched indexed search");
                    if !result.evidence.contains(&evidence) {
                        result.evidence.push(evidence);
                    }
                }
                let key = normalize_candidate_path(&result.path.to_string_lossy());
                match by_path.get_mut(&key) {
                    Some((best_rank, existing)) => {
                        for evidence in &result.evidence {
                            if !existing.evidence.contains(evidence) {
                                existing.evidence.push(evidence.clone());
                            }
                        }
                        merge_evidence_refs(&mut existing.evidence_refs, &result.evidence_refs);
                        if rank < *best_rank
                            || (rank == *best_rank
                                && result
                                    .score
                                    .partial_cmp(&existing.score)
                                    .unwrap_or(Ordering::Equal)
                                    .is_gt())
                        {
                            let mut replacement = result;
                            merge_evidence_refs(
                                &mut replacement.evidence_refs,
                                &existing.evidence_refs,
                            );
                            *existing = replacement;
                            *best_rank = rank;
                        }
                    }
                    None => {
                        by_path.insert(key, (rank, result));
                    }
                }
            }
        }
        let mut ranked = by_path.into_values().collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| {
                    right
                        .1
                        .score
                        .partial_cmp(&left.1.score)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| left.1.path.cmp(&right.1.path))
        });
        Ok(CandidateStream::success(
            RetrievalSourceKind::Lexical,
            ranked
                .into_iter()
                .map(|(_, result)| {
                    StreamCandidate::from_result(
                        result,
                        RetrievalAuthority::Heuristic,
                        "production lexical index ranked this file",
                    )
                })
                .collect(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct UnavailableCandidateSource {
    source: RetrievalSourceKind,
    caveat: String,
}

impl UnavailableCandidateSource {
    pub fn new(source: RetrievalSourceKind, caveat: impl Into<String>) -> Self {
        Self {
            source,
            caveat: caveat.into(),
        }
    }
}

impl ContextCandidateSource for UnavailableCandidateSource {
    fn source(&self) -> RetrievalSourceKind {
        self.source
    }

    fn retrieve(&self, _request: &CandidateRequest) -> Result<CandidateStream> {
        Ok(CandidateStream::unavailable(
            self.source,
            self.caveat.clone(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct StreamCandidate {
    pub result: SearchResult,
    pub raw_score: Option<f32>,
    pub authority: RetrievalAuthority,
    pub evidence_refs: Vec<String>,
    pub rationale: String,
}

impl StreamCandidate {
    pub fn from_result(
        result: SearchResult,
        authority: RetrievalAuthority,
        rationale: impl Into<String>,
    ) -> Self {
        let raw_score = Some(result.score);
        let evidence_refs = result.derived_evidence_ids();
        Self {
            result,
            raw_score,
            authority,
            evidence_refs,
            rationale: rationale.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CandidateStream {
    pub source: RetrievalSourceKind,
    pub candidates: Vec<StreamCandidate>,
    pub caveats: Vec<String>,
    /// Whether the source executed successfully. An available stream may legitimately return
    /// zero candidates; unavailable is reserved for source/config/index failures.
    pub available: bool,
}

impl CandidateStream {
    pub fn success(source: RetrievalSourceKind, candidates: Vec<StreamCandidate>) -> Self {
        Self {
            source,
            candidates,
            caveats: Vec::new(),
            available: true,
        }
    }

    pub fn unavailable(source: RetrievalSourceKind, caveat: impl Into<String>) -> Self {
        Self {
            source,
            candidates: Vec::new(),
            caveats: vec![caveat.into()],
            available: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FusionConfig {
    pub rrf_k: f32,
    pub source_weights: BTreeMap<RetrievalSourceKind, f32>,
}

impl FusionConfig {
    pub fn unweighted() -> Self {
        Self {
            rrf_k: DEFAULT_RRF_K,
            source_weights: all_retrieval_sources()
                .into_iter()
                .map(|source| (source, 1.0))
                .collect(),
        }
    }

    /// A predeclared evidence-prior profile retained for benchmark comparison. These weights are
    /// not calibration results and must not become the product default without frozen-corpus data.
    pub fn evidence_prior_weighted() -> Self {
        Self {
            rrf_k: DEFAULT_RRF_K,
            source_weights: BTreeMap::from([
                (RetrievalSourceKind::Lexical, 1.00),
                (RetrievalSourceKind::Document, 0.90),
                (RetrievalSourceKind::ExactSemantic, 1.50),
                (RetrievalSourceKind::Graph, 1.20),
                (RetrievalSourceKind::SemanticVector, 0.80),
                (RetrievalSourceKind::Validation, 1.05),
                (RetrievalSourceKind::GitHistory, 0.90),
                (RetrievalSourceKind::Runtime, 1.05),
            ]),
        }
    }
}

impl FusionConfig {
    /// Preserve repository ranking customization without re-applying the legacy score fusion.
    /// Default ranking weights normalize to 1.0, so the measured product default remains plain
    /// RRF. User overrides become relative per-source priors.
    pub fn from_ranking_options(options: &RankingOptions) -> Self {
        let defaults = RankingWeights::default();
        let mut config = Self::unweighted();
        if options.mode == RankingMode::Baseline {
            for weight in config.source_weights.values_mut() {
                *weight = 0.0;
            }
            config
                .source_weights
                .insert(RetrievalSourceKind::Lexical, 1.0);
            return config;
        }

        for (source, configured, baseline) in [
            (
                RetrievalSourceKind::Lexical,
                options.weights.text_relevance,
                defaults.text_relevance,
            ),
            (
                RetrievalSourceKind::ExactSemantic,
                options.weights.exact_reference,
                defaults.exact_reference,
            ),
            (
                RetrievalSourceKind::Graph,
                options.weights.graph_proximity,
                defaults.graph_proximity,
            ),
            (
                RetrievalSourceKind::SemanticVector,
                options.weights.semantic_similarity,
                defaults.semantic_similarity,
            ),
            (
                RetrievalSourceKind::Validation,
                options.weights.validation_proximity,
                defaults.validation_proximity,
            ),
            (
                RetrievalSourceKind::GitHistory,
                options.weights.git_cochange,
                defaults.git_cochange,
            ),
            (
                RetrievalSourceKind::Runtime,
                options.weights.runtime_corroboration,
                defaults.runtime_corroboration,
            ),
        ] {
            config
                .source_weights
                .insert(source, relative_source_weight(configured, baseline));
        }

        if let RankingMode::WithoutSignal(signal) = options.mode {
            if let Some(source) = source_for_ranking_signal(signal) {
                config.source_weights.insert(source, 0.0);
            }
        }
        config
    }
}

fn relative_source_weight(configured: f32, baseline: f32) -> f32 {
    if !configured.is_finite() || !baseline.is_finite() || baseline.abs() <= f32::EPSILON {
        return 1.0;
    }
    (configured / baseline).max(0.0)
}

fn source_for_ranking_signal(signal: RankingSignal) -> Option<RetrievalSourceKind> {
    match signal {
        RankingSignal::TextRelevance => Some(RetrievalSourceKind::Lexical),
        RankingSignal::ExactReference => Some(RetrievalSourceKind::ExactSemantic),
        RankingSignal::GraphProximity => Some(RetrievalSourceKind::Graph),
        RankingSignal::RuntimeCorroboration => Some(RetrievalSourceKind::Runtime),
        RankingSignal::GitCochange => Some(RetrievalSourceKind::GitHistory),
        RankingSignal::ValidationProximity => Some(RetrievalSourceKind::Validation),
        RankingSignal::SemanticSimilarity => Some(RetrievalSourceKind::SemanticVector),
        RankingSignal::BoundaryFit | RankingSignal::MemorySignal | RankingSignal::PathQuality => {
            None
        }
    }
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self::unweighted()
    }
}

fn all_retrieval_sources() -> [RetrievalSourceKind; 8] {
    [
        RetrievalSourceKind::Lexical,
        RetrievalSourceKind::Document,
        RetrievalSourceKind::ExactSemantic,
        RetrievalSourceKind::Graph,
        RetrievalSourceKind::SemanticVector,
        RetrievalSourceKind::Validation,
        RetrievalSourceKind::GitHistory,
        RetrievalSourceKind::Runtime,
    ]
}

#[derive(Debug, Clone)]
pub struct FusedCandidates {
    pub results: Vec<SearchResult>,
    pub diagnostics: RetrievalDiagnostics,
}

pub fn fuse_candidate_streams(
    streams: &[CandidateStream],
    limit: usize,
    config: &FusionConfig,
) -> FusedCandidates {
    let limit = limit.clamp(1, 200);
    let rrf_k = config.rrf_k.max(1.0);
    let mut by_path = BTreeMap::<String, FusedEntry>::new();
    let mut caveats = Vec::new();
    let mut attempted = BTreeSet::new();
    let mut succeeded = BTreeSet::new();

    for stream in streams {
        attempted.insert(stream.source);
        caveats.extend(stream.caveats.iter().cloned());
        if stream.available {
            succeeded.insert(stream.source);
        }
        let weight = config
            .source_weights
            .get(&stream.source)
            .copied()
            .unwrap_or(1.0)
            .max(0.0);
        if weight <= f32::EPSILON {
            caveats.push(format!(
                "{:?} candidate stream was disabled by ranking configuration",
                stream.source
            ));
            continue;
        }
        let deduped = dedupe_stream_candidates(&stream.candidates);
        for (index, candidate) in deduped.iter().enumerate() {
            let key = normalize_candidate_path(&candidate.result.path.to_string_lossy());
            let rank = index + 1;
            let rrf_contribution = weight / (rrf_k + rank as f32);
            let contribution = RetrievalContribution {
                source: stream.source,
                rank,
                raw_score: candidate.raw_score,
                rrf_contribution,
                authority: candidate.authority,
                symbol_id: candidate
                    .result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.id.clone()),
                evidence_refs: dedup_strings(candidate.evidence_refs.clone()),
                rationale: candidate.rationale.clone(),
            };
            let entry = by_path.entry(key).or_insert_with(|| FusedEntry {
                representative: candidate.result.clone(),
                fused_score: 0.0,
                authority: candidate.authority,
                contributions: Vec::new(),
                extra_evidence_refs: Vec::new(),
                best_rank: rank,
                best_authority: candidate.authority,
            });
            entry.fused_score += rrf_contribution;
            entry.authority = entry.authority.max(candidate.authority);
            entry.contributions.push(contribution);
            merge_evidence_refs(&mut entry.extra_evidence_refs, &candidate.evidence_refs);

            if candidate_preferred_as_representative(
                candidate.authority,
                rank,
                candidate.result.score,
                entry.best_authority,
                entry.best_rank,
                entry.representative.score,
            ) {
                entry.representative = candidate.result.clone();
                entry.best_rank = rank;
                entry.best_authority = candidate.authority;
            }
        }
    }

    let mut entries = by_path.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .authority
            .cmp(&left.authority)
            .then_with(|| {
                right
                    .fused_score
                    .partial_cmp(&left.fused_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| {
                normalize_candidate_path(&left.representative.path.to_string_lossy()).cmp(
                    &normalize_candidate_path(&right.representative.path.to_string_lossy()),
                )
            })
    });
    entries.truncate(limit);

    let mut results = Vec::with_capacity(entries.len());
    let mut traces = Vec::with_capacity(entries.len());
    for mut entry in entries {
        entry.contributions.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.rank.cmp(&right.rank))
                .then_with(|| left.rationale.cmp(&right.rationale))
        });
        entry.representative.score = entry.fused_score;
        merge_evidence_refs(
            &mut entry.representative.evidence_refs,
            &entry.extra_evidence_refs,
        );
        for contribution in &entry.contributions {
            let weight = config
                .source_weights
                .get(&contribution.source)
                .copied()
                .unwrap_or(1.0)
                .max(0.0);
            entry
                .representative
                .score_breakdown
                .push(ScoreComponent::new(
                    format!(
                        "retrieval_rrf:{}",
                        retrieval_source_label(contribution.source)
                    ),
                    contribution.raw_score.unwrap_or_default(),
                    1.0 / (rrf_k + contribution.rank as f32),
                    weight,
                    contribution.rrf_contribution,
                    contribution.evidence_refs.clone(),
                    format!(
                        "source rank {} with {:?} authority: {}",
                        contribution.rank, contribution.authority, contribution.rationale
                    ),
                ));
        }
        traces.push(RetrievalTrace {
            path: entry.representative.path.clone(),
            fused_score: entry.fused_score,
            authority: entry.authority,
            contributions: entry.contributions,
        });
        results.push(entry.representative);
    }

    caveats.sort();
    caveats.dedup();
    FusedCandidates {
        results,
        diagnostics: RetrievalDiagnostics {
            traces,
            caveats,
            sources_attempted: attempted.into_iter().collect(),
            sources_succeeded: succeeded.into_iter().collect(),
        },
    }
}

pub fn retrieve_candidate_streams(
    external_sources: &[&dyn ContextCandidateSource],
    request: &CandidateRequest,
) -> Vec<CandidateStream> {
    external_sources
        .iter()
        .map(|source| {
            let expected = source.source();
            match source.retrieve(request) {
                Ok(stream) if stream.source == expected => stream,
                Ok(stream) => CandidateStream::unavailable(
                    expected,
                    format!(
                        "candidate source contract mismatch: {:?} returned {:?}",
                        expected, stream.source
                    ),
                ),
                Err(err) => CandidateStream::unavailable(
                    expected,
                    format!("{:?} candidate stream unavailable: {err}", expected),
                ),
            }
        })
        .collect()
}

pub fn retrieve_and_fuse_candidate_streams(
    builtins: Vec<CandidateStream>,
    external_sources: &[&dyn ContextCandidateSource],
    request: &CandidateRequest,
    limit: usize,
    config: &FusionConfig,
) -> FusedCandidates {
    let external = retrieve_candidate_streams(external_sources, request);
    let overridden = external
        .iter()
        .filter(|stream| stream.available)
        .map(|stream| stream.source)
        .collect::<BTreeSet<_>>();
    let mut streams = builtins
        .into_iter()
        .filter(|stream| !overridden.contains(&stream.source))
        .collect::<Vec<_>>();
    streams.extend(external);
    fuse_candidate_streams(&streams, limit, config)
}

pub(crate) fn retrieval_source_label(source: RetrievalSourceKind) -> &'static str {
    match source {
        RetrievalSourceKind::Lexical => "lexical",
        RetrievalSourceKind::Document => "document",
        RetrievalSourceKind::ExactSemantic => "exact_semantic",
        RetrievalSourceKind::Graph => "graph",
        RetrievalSourceKind::SemanticVector => "semantic_vector",
        RetrievalSourceKind::Validation => "validation",
        RetrievalSourceKind::GitHistory => "git_history",
        RetrievalSourceKind::Runtime => "runtime",
    }
}

#[derive(Debug, Clone)]
struct FusedEntry {
    representative: SearchResult,
    fused_score: f32,
    authority: RetrievalAuthority,
    contributions: Vec<RetrievalContribution>,
    extra_evidence_refs: Vec<String>,
    best_rank: usize,
    best_authority: RetrievalAuthority,
}

fn dedupe_stream_candidates(candidates: &[StreamCandidate]) -> Vec<StreamCandidate> {
    let mut deduped: Vec<StreamCandidate> = Vec::new();
    let mut positions = BTreeMap::<String, usize>::new();

    for candidate in candidates {
        let key = normalize_candidate_path(&candidate.result.path.to_string_lossy());
        if let Some(index) = positions.get(&key).copied() {
            let existing = &mut deduped[index];
            merge_evidence_refs(&mut existing.evidence_refs, &candidate.evidence_refs);
            merge_evidence_refs(&mut existing.result.evidence, &candidate.result.evidence);
            merge_evidence_refs(
                &mut existing.result.evidence_refs,
                &candidate.result.evidence_refs,
            );
            existing.result.confidence =
                existing.result.confidence.max(candidate.result.confidence);

            // Preserve the first file rank/raw score from the source, but do not lose stronger
            // authority or exact symbol identity carried by a later chunk for the same file.
            if candidate.authority > existing.authority {
                existing.authority = candidate.authority;
                existing.result.match_reason = candidate.result.match_reason.clone();
                if candidate.result.symbol.is_some() {
                    existing.result.symbol = candidate.result.symbol.clone();
                }
            }
            if candidate.rationale != existing.rationale
                && !existing.rationale.contains(&candidate.rationale)
            {
                existing.rationale.push_str("; ");
                existing.rationale.push_str(&candidate.rationale);
            }
            continue;
        }

        positions.insert(key, deduped.len());
        deduped.push(candidate.clone());
    }

    deduped
}

fn candidate_preferred_as_representative(
    authority: RetrievalAuthority,
    rank: usize,
    score: f32,
    current_authority: RetrievalAuthority,
    current_rank: usize,
    current_score: f32,
) -> bool {
    authority > current_authority
        || (authority == current_authority && rank < current_rank)
        || (authority == current_authority
            && rank == current_rank
            && score
                .partial_cmp(&current_score)
                .unwrap_or(Ordering::Equal)
                .is_gt())
}

fn merge_evidence_refs(target: &mut Vec<String>, incoming: &[String]) {
    for evidence in incoming {
        if !target.iter().any(|existing| existing == evidence) {
            target.push(evidence.clone());
        }
    }
    target.sort();
}

fn dedup_strings(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn normalize_candidate_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn is_document_candidate_path(path: &str) -> bool {
    let path = normalize_candidate_path(path).to_ascii_lowercase();
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.ends_with("readme.md")
        || path.ends_with("readme.mdx")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EdgeId, EvidenceSourceType, FileId, GraphEdge, GraphEdgeType, Language,
        LineRange, NodeId, Symbol, SymbolId, SymbolKind, Visibility,
    };
    use std::path::{Path, PathBuf};

    fn result(path: &str, score: f32, symbol: Option<&str>) -> SearchResult {
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange { start: 1, end: 4 }),
            snippet: format!("snippet {path}"),
            symbol: symbol.map(|name| Symbol {
                id: SymbolId::new(format!("symbol:{name}")),
                name: name.into(),
                qualified_name: format!("fixture::{name}"),
                kind: SymbolKind::Function,
                file_id: FileId::new(format!("file:{path}")),
                range: Some(LineRange { start: 1, end: 4 }),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: Visibility::Unknown,
            }),
            score,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: vec![format!("evidence:{path}")],
            confidence: 0.8,
            score_breakdown: Vec::new(),
        }
    }

    fn candidate(
        path: &str,
        score: f32,
        authority: RetrievalAuthority,
        symbol: Option<&str>,
    ) -> StreamCandidate {
        StreamCandidate::from_result(result(path, score, symbol), authority, "fixture candidate")
    }

    #[test]
    fn default_fusion_is_unweighted_until_calibration_is_benchmarked() {
        let config = FusionConfig::default();
        assert!(config
            .source_weights
            .values()
            .all(|weight| (*weight - 1.0).abs() < f32::EPSILON));
        let weighted = FusionConfig::evidence_prior_weighted();
        assert_ne!(config.source_weights, weighted.source_weights);
    }

    #[test]
    fn default_ranking_options_preserve_unweighted_rrf() {
        let config = FusionConfig::from_ranking_options(&RankingOptions::default());
        assert!(config
            .source_weights
            .values()
            .all(|weight| (*weight - 1.0).abs() < f32::EPSILON));
    }

    #[test]
    fn repository_ranking_overrides_become_relative_source_priors() {
        let mut options = RankingOptions::default();
        options.weights.semantic_similarity *= 2.0;
        options.weights.graph_proximity *= 0.5;
        let config = FusionConfig::from_ranking_options(&options);
        assert_eq!(
            config.source_weights[&RetrievalSourceKind::SemanticVector],
            2.0
        );
        assert_eq!(config.source_weights[&RetrievalSourceKind::Graph], 0.5);
        assert_eq!(config.source_weights[&RetrievalSourceKind::Lexical], 1.0);
    }

    #[test]
    fn baseline_and_signal_ablation_modes_disable_sources_without_zero_weight_votes() {
        let baseline = FusionConfig::from_ranking_options(&RankingOptions {
            mode: RankingMode::Baseline,
            ..RankingOptions::default()
        });
        assert_eq!(baseline.source_weights[&RetrievalSourceKind::Lexical], 1.0);
        assert!(baseline
            .source_weights
            .iter()
            .all(|(source, weight)| *source == RetrievalSourceKind::Lexical || *weight == 0.0));

        let without_semantic = FusionConfig::from_ranking_options(&RankingOptions {
            mode: RankingMode::WithoutSignal(RankingSignal::SemanticSimilarity),
            ..RankingOptions::default()
        });
        assert_eq!(
            without_semantic.source_weights[&RetrievalSourceKind::SemanticVector],
            0.0
        );
    }

    #[test]
    fn zero_weight_streams_do_not_contribute_candidates() {
        let streams = vec![CandidateStream::success(
            RetrievalSourceKind::SemanticVector,
            vec![candidate(
                "src/semantic.rs",
                1.0,
                RetrievalAuthority::Heuristic,
                Some("semantic"),
            )],
        )];
        let mut config = FusionConfig::unweighted();
        config
            .source_weights
            .insert(RetrievalSourceKind::SemanticVector, 0.0);
        let fused = fuse_candidate_streams(&streams, 10, &config);
        assert!(fused.results.is_empty());
        assert!(fused
            .diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("disabled by ranking configuration")));
    }

    #[test]
    fn rrf_combines_independent_ranks_without_normalizing_raw_scores() {
        let streams = vec![
            CandidateStream::success(
                RetrievalSourceKind::Lexical,
                vec![
                    candidate("src/a.rs", 100.0, RetrievalAuthority::Heuristic, Some("a")),
                    candidate("src/b.rs", 0.9, RetrievalAuthority::Heuristic, Some("b")),
                ],
            ),
            CandidateStream::success(
                RetrievalSourceKind::Graph,
                vec![
                    candidate(
                        "src/b.rs",
                        0.1,
                        RetrievalAuthority::Corroborating,
                        Some("b"),
                    ),
                    candidate(
                        "src/a.rs",
                        5000.0,
                        RetrievalAuthority::Corroborating,
                        Some("a"),
                    ),
                ],
            ),
        ];

        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        assert_eq!(fused.results.len(), 2);
        // Raw scores are intentionally incomparable; symmetric source ranks tie under plain RRF.
        // Deterministic path ordering resolves the tie rather than an arbitrary raw-score scale.
        assert_eq!(fused.results[0].path, PathBuf::from("src/a.rs"));
        assert_eq!(fused.diagnostics.traces[0].contributions.len(), 2);
        assert!(
            (fused.diagnostics.traces[0].fused_score - fused.diagnostics.traces[1].fused_score)
                .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn one_stream_gets_only_one_vote_per_file() {
        let streams = vec![CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![
                candidate("src/a.rs", 1.0, RetrievalAuthority::Heuristic, Some("a1")),
                candidate("src/a.rs", 0.9, RetrievalAuthority::Heuristic, Some("a2")),
                candidate("src/b.rs", 0.8, RetrievalAuthority::Heuristic, Some("b")),
            ],
        )];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        let a = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/a.rs"))
            .unwrap();
        let b = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/b.rs"))
            .unwrap();
        assert_eq!(a.contributions.len(), 1);
        assert_eq!(a.contributions[0].rank, 1);
        assert_eq!(b.contributions.len(), 1);
        assert_eq!(b.contributions[0].rank, 2);
    }

    #[test]
    fn duplicate_file_candidates_merge_stronger_authority_without_extra_vote() {
        let streams = vec![CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![
                candidate("src/a.rs", 1.0, RetrievalAuthority::Heuristic, Some("a1")),
                candidate("src/a.rs", 0.9, RetrievalAuthority::Exact, Some("ExactA")),
                candidate("src/b.rs", 0.8, RetrievalAuthority::Heuristic, Some("b")),
            ],
        )];

        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        let a_trace = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/a.rs"))
            .unwrap();
        let b_trace = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/b.rs"))
            .unwrap();
        let a_result = fused
            .results
            .iter()
            .find(|result| result.path == Path::new("src/a.rs"))
            .unwrap();

        assert_eq!(a_trace.authority, RetrievalAuthority::Exact);
        assert_eq!(a_trace.contributions.len(), 1);
        assert_eq!(a_trace.contributions[0].rank, 1);
        assert_eq!(b_trace.contributions[0].rank, 2);
        assert_eq!(
            a_result.symbol.as_ref().map(|symbol| symbol.name.as_str()),
            Some("ExactA")
        );
    }

    #[test]
    fn authority_is_preserved_independently_from_fused_relevance() {
        let streams = vec![
            CandidateStream::success(
                RetrievalSourceKind::SemanticVector,
                vec![candidate(
                    "src/semantic.rs",
                    0.99,
                    RetrievalAuthority::Heuristic,
                    Some("semantic"),
                )],
            ),
            CandidateStream::success(
                RetrievalSourceKind::ExactSemantic,
                vec![candidate(
                    "src/exact.rs",
                    0.2,
                    RetrievalAuthority::Exact,
                    Some("exact"),
                )],
            ),
        ];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        let exact = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/exact.rs"))
            .unwrap();
        let semantic = fused
            .diagnostics
            .traces
            .iter()
            .find(|trace| trace.path == Path::new("src/semantic.rs"))
            .unwrap();
        assert_eq!(exact.authority, RetrievalAuthority::Exact);
        assert_eq!(semantic.authority, RetrievalAuthority::Heuristic);
        assert!((exact.fused_score - semantic.fused_score).abs() < f32::EPSILON);
    }

    #[derive(Clone)]
    struct FixtureSearchIndex {
        results: Vec<SearchResult>,
    }

    impl open_kioku_storage::SearchIndex for FixtureSearchIndex {
        fn rebuild(
            &mut self,
            _chunks: &[open_kioku_core::CodeChunk],
            _files: &[open_kioku_core::File],
            _symbols: &[Symbol],
        ) -> Result<()> {
            Ok(())
        }

        fn search(&self, _query: &str, _limit: usize) -> Result<Vec<SearchResult>> {
            Ok(self.results.clone())
        }
    }

    #[test]
    fn search_index_source_collapses_same_file_to_one_ranked_vote() {
        let index = FixtureSearchIndex {
            results: vec![
                result("docs/guide.md", 1.0, None),
                result("src/a.rs", 0.9, Some("a")),
                result("src/a.rs", 0.8, Some("a2")),
                result("src/b.rs", 0.7, Some("b")),
            ],
        };
        let source = SearchIndexCandidateSource::new(index);
        let request = CandidateRequest::new("change auth", vec!["change auth".into()], 10);
        let stream = source.retrieve(&request).unwrap();
        assert!(stream.available);
        assert_eq!(stream.source, RetrievalSourceKind::Lexical);
        assert_eq!(stream.candidates.len(), 2);
        assert_eq!(stream.candidates[0].result.path, PathBuf::from("src/a.rs"));
        assert_eq!(stream.candidates[1].result.path, PathBuf::from("src/b.rs"));
    }

    #[derive(Clone)]
    struct FixtureSource {
        source: RetrievalSourceKind,
        result: std::result::Result<CandidateStream, String>,
    }

    impl ContextCandidateSource for FixtureSource {
        fn source(&self) -> RetrievalSourceKind {
            self.source
        }

        fn retrieve(&self, _request: &CandidateRequest) -> Result<CandidateStream> {
            self.result
                .clone()
                .map_err(open_kioku_errors::OkError::Search)
        }
    }

    #[test]
    fn external_source_replaces_same_kind_without_duplicate_vote() {
        let builtins = vec![CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![candidate(
                "src/builtin.rs",
                1.0,
                RetrievalAuthority::Heuristic,
                Some("builtin"),
            )],
        )];
        let external = FixtureSource {
            source: RetrievalSourceKind::Lexical,
            result: Ok(CandidateStream::success(
                RetrievalSourceKind::Lexical,
                vec![candidate(
                    "src/external.rs",
                    1.0,
                    RetrievalAuthority::Heuristic,
                    Some("external"),
                )],
            )),
        };
        let request = CandidateRequest::new("external", vec!["external".into()], 10);
        let fused = retrieve_and_fuse_candidate_streams(
            builtins,
            &[&external],
            &request,
            10,
            &FusionConfig::default(),
        );
        assert_eq!(fused.results.len(), 1);
        assert_eq!(fused.results[0].path, PathBuf::from("src/external.rs"));
        assert_eq!(fused.diagnostics.traces[0].contributions.len(), 1);
    }

    #[test]
    fn external_source_failure_is_a_caveat_not_a_global_failure() {
        let builtins = vec![CandidateStream::success(
            RetrievalSourceKind::ExactSemantic,
            vec![candidate(
                "src/exact.rs",
                1.0,
                RetrievalAuthority::Exact,
                Some("exact"),
            )],
        )];
        let failing = FixtureSource {
            source: RetrievalSourceKind::SemanticVector,
            result: Err("semantic index is stale".into()),
        };
        let request = CandidateRequest::new("exact", vec!["exact".into()], 10);
        let fused = retrieve_and_fuse_candidate_streams(
            builtins,
            &[&failing],
            &request,
            10,
            &FusionConfig::default(),
        );
        assert_eq!(fused.results.len(), 1);
        assert_eq!(fused.results[0].path, PathBuf::from("src/exact.rs"));
        assert!(fused
            .diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("stale")));
    }

    #[test]
    fn failed_external_source_keeps_healthy_builtin_fallback() {
        let builtins = vec![CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![candidate(
                "src/fallback.rs",
                1.0,
                RetrievalAuthority::Heuristic,
                Some("fallback"),
            )],
        )];
        let failing = FixtureSource {
            source: RetrievalSourceKind::Lexical,
            result: Err("tantivy index could not be opened".into()),
        };
        let request = CandidateRequest::new("fallback", vec!["fallback".into()], 10);
        let fused = retrieve_and_fuse_candidate_streams(
            builtins,
            &[&failing],
            &request,
            10,
            &FusionConfig::default(),
        );
        assert_eq!(fused.results.len(), 1);
        assert_eq!(fused.results[0].path, PathBuf::from("src/fallback.rs"));
        assert!(fused
            .diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("could not be opened")));
        assert!(fused
            .diagnostics
            .sources_succeeded
            .contains(&RetrievalSourceKind::Lexical));
    }

    #[test]
    fn fused_results_carry_source_contributions_in_score_breakdown() {
        let streams = vec![CandidateStream::success(
            RetrievalSourceKind::SemanticVector,
            vec![candidate(
                "src/a.rs",
                0.91,
                RetrievalAuthority::Heuristic,
                Some("a"),
            )],
        )];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        assert!(fused.results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "retrieval_rrf:semantic_vector"));
        assert_eq!(
            fused.diagnostics.traces[0].authority,
            RetrievalAuthority::Heuristic
        );
    }

    #[test]
    fn graph_evidence_refs_only_include_edges_incident_to_the_candidate() {
        let anchor = NodeId::new("symbol:anchor");
        let candidate = NodeId::new("symbol:candidate");
        let other = NodeId::new("symbol:other");
        let edges = vec![
            GraphEdge {
                id: EdgeId::new("edge:direct"),
                from: anchor.clone(),
                to: candidate.clone(),
                edge_type: GraphEdgeType::Calls,
                ..Default::default()
            },
            GraphEdge {
                id: EdgeId::new("edge:other"),
                from: anchor.clone(),
                to: other.clone(),
                edge_type: GraphEdgeType::Calls,
                ..Default::default()
            },
            GraphEdge {
                id: EdgeId::new("edge:unrelated"),
                from: candidate.clone(),
                to: other,
                edge_type: GraphEdgeType::Calls,
                ..Default::default()
            },
        ];
        assert_eq!(
            builtins::incident_edge_ids(&anchor, &candidate, &edges),
            vec!["edge:direct"]
        );
    }

    #[test]
    fn task_adjustments_preserve_rrf_source_provenance() {
        let streams = vec![CandidateStream::success(
            RetrievalSourceKind::ExactSemantic,
            vec![candidate(
                "src/payment.rs",
                1.0,
                RetrievalAuthority::Exact,
                Some("PaymentService"),
            )],
        )];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        let intent = crate::TaskSearchIntent::parse("change PaymentService");
        let diagnostics = fused.diagnostics.clone();
        let ranked = crate::rerank_fused_for_task(fused.results, &intent, &diagnostics);
        let signals = ranked[0]
            .score_breakdown
            .iter()
            .map(|component| component.signal.as_str())
            .collect::<Vec<_>>();
        assert!(signals.contains(&"retrieval_rrf:exact_semantic"));
        assert!(signals.contains(&"primary_task_anchor_boost"));
    }

    #[test]
    fn exact_evidence_outranks_more_popular_heuristic_evidence() {
        let streams = vec![
            CandidateStream::success(
                RetrievalSourceKind::ExactSemantic,
                vec![candidate(
                    "src/exact.rs",
                    1.0,
                    RetrievalAuthority::Exact,
                    Some("ExactTarget"),
                )],
            ),
            CandidateStream::success(
                RetrievalSourceKind::Lexical,
                vec![candidate(
                    "src/heuristic.rs",
                    1.0,
                    RetrievalAuthority::Heuristic,
                    Some("HeuristicTarget"),
                )],
            ),
            CandidateStream::success(
                RetrievalSourceKind::SemanticVector,
                vec![candidate(
                    "src/heuristic.rs",
                    0.99,
                    RetrievalAuthority::Heuristic,
                    Some("HeuristicTarget"),
                )],
            ),
            CandidateStream::success(
                RetrievalSourceKind::GitHistory,
                vec![candidate(
                    "src/heuristic.rs",
                    0.98,
                    RetrievalAuthority::Heuristic,
                    Some("HeuristicTarget"),
                )],
            ),
        ];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::unweighted());
        assert_eq!(fused.results[0].path, PathBuf::from("src/exact.rs"));
        assert_eq!(
            fused.diagnostics.traces[0].authority,
            RetrievalAuthority::Exact
        );
        assert!(fused.diagnostics.traces[1].fused_score > fused.diagnostics.traces[0].fused_score);
    }

    #[test]
    fn unavailable_streams_degrade_with_explicit_caveats() {
        let streams = vec![
            CandidateStream::success(
                RetrievalSourceKind::Lexical,
                vec![candidate(
                    "src/a.rs",
                    1.0,
                    RetrievalAuthority::Heuristic,
                    Some("a"),
                )],
            ),
            CandidateStream::unavailable(
                RetrievalSourceKind::SemanticVector,
                "semantic vector index is disabled",
            ),
        ];
        let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::default());
        assert_eq!(fused.results.len(), 1);
        assert!(fused
            .diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("disabled")));
        assert!(fused
            .diagnostics
            .sources_attempted
            .contains(&RetrievalSourceKind::SemanticVector));
        assert!(!fused
            .diagnostics
            .sources_succeeded
            .contains(&RetrievalSourceKind::SemanticVector));
    }
}
