use open_kioku_core::{ScoreComponent, SearchResult};
use std::borrow::Cow;

/// Signals a lexical producer attaches to the score it wrote into `SearchResult.score`: the
/// Tantivy index's `bm25_relevance` and the in-memory fallback's `lexical_relevance`. Only
/// candidates carrying one of them set a scaled `text_relevance`.
const LEXICAL_ORIGIN_SIGNALS: [&str; 2] = ["bm25_relevance", "lexical_relevance"];

/// The components a lexical producer's `SearchResult.score` is made of: the base score and
/// Tantivy's query-variant boost. Kept at weight 0 beside a scaled `text_relevance`, since
/// the boosted score is what gets scaled.
const LEXICAL_SCORE_PARTS: [&str; 3] =
    ["bm25_relevance", "lexical_relevance", "query_variant_boost"];

const TEXT_RELEVANCE_RATIONALE: &str = "BM25 or lexical score from indexed text";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingWeights {
    pub text_relevance: f32,
    pub exact_reference: f32,
    pub graph_proximity: f32,
    pub boundary_fit: f32,
    pub runtime_corroboration: f32,
    pub git_cochange: f32,
    pub validation_proximity: f32,
    pub memory_signal: f32,
    pub path_quality: f32,
    pub semantic_similarity: f32,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            text_relevance: 1.0,
            exact_reference: 1.0,
            graph_proximity: 0.35,
            boundary_fit: 0.25,
            runtime_corroboration: 0.30,
            git_cochange: 0.25,
            validation_proximity: 1.0,
            memory_signal: 0.20,
            path_quality: 1.0,
            semantic_similarity: 0.30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingMode {
    Baseline,
    Fusion,
    WithoutSignal(RankingSignal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingSignal {
    TextRelevance,
    ExactReference,
    GraphProximity,
    BoundaryFit,
    RuntimeCorroboration,
    GitCochange,
    ValidationProximity,
    MemorySignal,
    PathQuality,
    SemanticSimilarity,
}

/// How `text_relevance` is put on the scale of the signals it is summed with.
///
/// `Raw` is what every shipped surface ranks with. The lexical score is unbounded boosted
/// BM25 while the other signals are bounded, so a bounded signal can only reorder near-ties.
/// The two scaled forms exist so the retrieval benchmark can measure them as advisory arms
/// before any default moves. None of them is an `ok.toml` setting. `Baseline` mode never
/// scales, whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TextRelevanceScale {
    #[default]
    Raw,
    /// Divided by the highest lexical score in the pool, so the top lexical hit reads 1.0.
    /// A positive per-query constant: with every other weight at zero the order is the raw
    /// order, and it does not depend on how deep the pool was fetched or on corpus size.
    PoolMax,
    /// `(k + 1) / (k + rank)`, where rank counts the pool's strictly higher lexical scores,
    /// so equal scores share a rank and the path tie-break cannot leak into the value.
    Rank { k: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankingOptions {
    pub weights: RankingWeights,
    pub mode: RankingMode,
    pub query: Option<String>,
    pub text_relevance_scale: TextRelevanceScale,
}

impl Default for RankingOptions {
    fn default() -> Self {
        Self {
            weights: RankingWeights::default(),
            mode: RankingMode::Fusion,
            query: None,
            text_relevance_scale: TextRelevanceScale::Raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankingFeatures {
    pub text_relevance: f32,
    pub exact_reference: f32,
    pub graph_proximity: f32,
    pub boundary_fit: f32,
    pub runtime_corroboration: f32,
    pub git_cochange: f32,
    pub history_churn: f32,
    pub ownership_risk: f32,
    pub similar_change_overlap: f32,
    pub reviewer_affinity: f32,
    pub validation_proximity: f32,
    pub memory_signal: f32,
    pub path_quality_penalty: f32,
    pub semantic_similarity: f32,
}

struct SignalSpec<'a> {
    signal: RankingSignal,
    name: &'a str,
    raw_value: f32,
    weight: f32,
    evidence_ids: Vec<String>,
    rationale: &'a str,
}

impl RankingFeatures {
    pub fn from_result(result: &SearchResult, query: Option<&str>) -> Self {
        Self::assess(result, query).0
    }

    /// The features, and whether the result is semantic-only. A scaled `text_relevance` is set
    /// by, and applied to, only candidates that carry a lexical component and are not
    /// semantic-only; every other candidate gets none.
    fn assess(result: &SearchResult, query: Option<&str>) -> (Self, bool) {
        let path = result.path.to_string_lossy().to_ascii_lowercase();
        // Every signal below reads a persisted `ScoreComponent` emitted by the
        // producer that actually holds the evidence. It must never be inferred
        // from evidence prose.
        //
        // These used to be substring probes over `evidence`. Tantivy writes the
        // user's own query into that prose -- `query variant `{variant}` matched
        // local index` -- so a search for the word "trace" scored itself
        // `runtime_corroboration`, on repositories with no runtime facts at all,
        // and attached the rationale "runtime traces or incidents near the
        // result". The same leak armed graph, memory, exact-reference and
        // co-change scoring for any query containing those ordinary words.
        // Absence was being rendered as presence, which is the one thing this
        // ranker must not do.
        //
        // An impact occurrence result carries no `exact_reference` component; its typed
        // provenance stands in for one. `match_reason` is not read: deduplication hands it to
        // whichever duplicate scored higher, and any result may say "exact symbol reference".
        let exact_reference = component_signal_value(result, &["exact_reference"])
            .or_else(|| result.is_exact_reference().then_some(0.35))
            .unwrap_or(0.0);
        let graph_proximity = component_signal_value(result, &["graph_proximity"]).unwrap_or(0.0);
        let boundary_fit = boundary_fit_score(result, &path, query);
        let runtime_corroboration =
            component_signal_value(result, &["runtime_corroboration"]).unwrap_or(0.0);
        let git_cochange =
            component_signal_value(result, &["git_cochange", "cochange"]).unwrap_or(0.0);
        let history_churn = component_signal_value(result, &["history_churn"]).unwrap_or(0.0);
        let ownership_risk = component_signal_value(result, &["ownership_risk"]).unwrap_or(0.0);
        let similar_change_overlap =
            component_signal_value(result, &["similar_change_overlap"]).unwrap_or(0.0);
        let reviewer_affinity =
            component_signal_value(result, &["reviewer_affinity"]).unwrap_or(0.0);
        // Test detection reads the original case: `internalClusterTest` and `FooIT.java` are
        // recognised at a CamelCase boundary that the lowercased `path` above has erased.
        let validation_proximity = if is_test_path(&result.path.to_string_lossy()) {
            0.05
        } else {
            0.0
        };
        let memory_signal = component_signal_value(result, &["memory_signal"]).unwrap_or(0.0);
        let semantic_similarity = result
            .score_breakdown
            .iter()
            .find(|component| {
                component.signal == "semantic_similarity"
                    || component.signal == "local_semantic_similarity"
            })
            .map(|component| component.raw_value)
            .unwrap_or(0.0);
        let semantic_only = semantic_similarity > 0.0
            && exact_reference <= 0.0
            && graph_proximity <= 0.0
            && runtime_corroboration <= 0.0
            && git_cochange <= 0.0
            && validation_proximity <= 0.0;
        let text_relevance = if semantic_only { 0.0 } else { result.score };
        let path_quality_penalty = path_quality_penalty(&path, result.score);
        let symbol_name_hit = query
            .filter(|query| exact_identity_match(result, query))
            .map(|_| 1.0)
            .unwrap_or_default();

        let features = Self {
            text_relevance,
            exact_reference: exact_reference + symbol_name_hit,
            graph_proximity,
            boundary_fit,
            runtime_corroboration,
            git_cochange,
            history_churn,
            ownership_risk,
            similar_change_overlap,
            reviewer_affinity,
            validation_proximity,
            memory_signal,
            path_quality_penalty,
            semantic_similarity,
        };
        (features, semantic_only)
    }
}

pub fn rerank(results: Vec<SearchResult>) -> Vec<SearchResult> {
    rerank_with_options(results, &RankingOptions::default())
}

pub fn rerank_baseline(results: Vec<SearchResult>) -> Vec<SearchResult> {
    rerank_with_options(
        results,
        &RankingOptions {
            weights: RankingWeights::default(),
            mode: RankingMode::Baseline,
            query: None,
            text_relevance_scale: TextRelevanceScale::Raw,
        },
    )
}

pub fn rerank_without_signal(
    results: Vec<SearchResult>,
    signal: RankingSignal,
) -> Vec<SearchResult> {
    rerank_with_options(
        results,
        &RankingOptions {
            weights: RankingWeights::default(),
            mode: RankingMode::WithoutSignal(signal),
            query: None,
            text_relevance_scale: TextRelevanceScale::Raw,
        },
    )
}

pub fn rerank_with_options(
    mut results: Vec<SearchResult>,
    options: &RankingOptions,
) -> Vec<SearchResult> {
    match options.mode {
        RankingMode::Baseline => {
            for result in &mut results {
                result.reconcile_score_breakdown();
            }
        }
        RankingMode::Fusion | RankingMode::WithoutSignal(_) => {
            let query = options.query.as_deref();
            let assessed = results
                .iter()
                .map(|result| RankingFeatures::assess(result, query))
                .collect::<Vec<_>>();
            let text_scale = TextScale::for_pool(options.text_relevance_scale, &results, &assessed);
            for (result, (features, semantic_only)) in results.iter_mut().zip(assessed) {
                let input = text_scale.fusion_input(result, features, semantic_only);
                apply_fusion(result, options, input);
            }
        }
    }
    results.sort_by(|a, b| {
        let (a_exact, b_exact) = options
            .query
            .as_deref()
            .map(|query| {
                (
                    exact_identity_match(a, query),
                    exact_identity_match(b, query),
                )
            })
            .unwrap_or_default();
        b_exact
            .cmp(&a_exact)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.path.cmp(&b.path))
    });
    results
}

pub fn top_score_signals(result: &SearchResult, limit: usize) -> Vec<String> {
    let mut components = result.score_breakdown.clone();
    components.sort_by(|a, b| {
        b.contribution
            .abs()
            .partial_cmp(&a.contribution.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    components
        .into_iter()
        .filter(|component| component.contribution.abs() > 0.001)
        .take(limit)
        .map(|component| format!("{} {:+.3}", component.signal, component.contribution))
        .collect()
}

/// The pool-level half of a scaled `text_relevance`, read from the candidates as retrieved,
/// before fusion rewrites their breakdowns.
struct TextScale {
    scale: TextRelevanceScale,
    /// Positive, finite scores of lexical-origin candidates that are not semantic-only, highest
    /// first. Always empty for `Raw`.
    lexical_scores: Vec<f32>,
}

/// What `apply_fusion` sums for one candidate once its text scale is settled.
struct FusionInput {
    features: RankingFeatures,
    text_rationale: Cow<'static, str>,
    /// The producer's lexical components, kept at zero weight when `text_relevance` is scaled,
    /// so the unscaled score stays in the breakdown without being counted twice.
    retained: Vec<ScoreComponent>,
}

impl FusionInput {
    fn unscaled(features: RankingFeatures, text_rationale: Cow<'static, str>) -> Self {
        Self {
            features,
            text_rationale,
            retained: Vec::new(),
        }
    }
}

impl TextScale {
    fn for_pool(
        scale: TextRelevanceScale,
        results: &[SearchResult],
        assessed: &[(RankingFeatures, bool)],
    ) -> Self {
        let mut lexical_scores = match scale {
            TextRelevanceScale::Raw => Vec::new(),
            TextRelevanceScale::PoolMax | TextRelevanceScale::Rank { .. } => results
                .iter()
                .zip(assessed)
                .filter_map(|(result, (_, semantic_only))| {
                    (!semantic_only && is_lexical_origin(result)).then_some(result.score)
                })
                .filter(|score| score.is_finite() && *score > 0.0)
                .collect(),
        };
        lexical_scores.sort_by(|left, right| right.total_cmp(left));
        Self {
            scale,
            lexical_scores,
        }
    }

    fn fusion_input(
        &self,
        result: &SearchResult,
        mut features: RankingFeatures,
        semantic_only: bool,
    ) -> FusionInput {
        let rank_k = match self.scale {
            TextRelevanceScale::Raw => {
                return FusionInput::unscaled(features, TEXT_RELEVANCE_RATIONALE.into())
            }
            TextRelevanceScale::PoolMax => None,
            TextRelevanceScale::Rank { k } => Some(k),
        };
        let score = if result.score.is_finite() {
            result.score
        } else {
            0.0
        };
        let path = result.path.to_string_lossy().to_ascii_lowercase();
        // Only a candidate that joined the lexical pool is scaled against it. A git co-change
        // candidate, or a semantic hit that a test path keeps from being semantic-only, would
        // otherwise take a share of a maximum, or a rank, it never contributed to.
        if semantic_only || !is_lexical_origin(result) {
            let reason = if semantic_only {
                "it is semantic-only"
            } else {
                "it carries no lexical component"
            };
            features.text_relevance = 0.0;
            // Its own score is a similarity or history score, bounded like the signals it is
            // summed with, so the penalty takes a bounded share of it.
            features.path_quality_penalty = path_quality_penalty(&path, score.clamp(0.0, 1.0));
            return FusionInput {
                features,
                text_rationale: TEXT_RELEVANCE_RATIONALE.into(),
                retained: vec![ScoreComponent::new(
                    "text_relevance_excluded",
                    score,
                    0.0,
                    0.0,
                    0.0,
                    result.derived_evidence_ids(),
                    format!(
                        "no text relevance: {reason}, so it neither sets nor takes the pool's lexical scale"
                    ),
                )],
            };
        }
        // A pool with no positive lexical score has nothing to scale by. Borrowing a zero
        // maximum would invent a scale, so the raw score stays, and a weight-0 component says
        // why even when that score is 0 and no `text_relevance` component is written.
        let Some(&max) = self.lexical_scores.first() else {
            let mut input = FusionInput::unscaled(
                features,
                "BM25 or lexical score from indexed text, left unscaled: no candidate in the pool carries a positive lexical score".into(),
            );
            input.retained.push(ScoreComponent::new(
                "text_relevance_unscaled",
                score,
                score.clamp(-1.0, 1.0),
                0.0,
                0.0,
                result.derived_evidence_ids(),
                "left unscaled: no candidate in the pool carries a positive lexical score",
            ));
            return input;
        };
        let (scaled, text_rationale, scale_component) = match rank_k {
            None => {
                let scaled = score / max;
                (
                    scaled,
                    format!(
                        "{TEXT_RELEVANCE_RATIONALE}: the boosted lexical score divided by the pool's top lexical score ({score} / {max})"
                    ),
                    ScoreComponent::new(
                        "text_relevance_pool_max",
                        max,
                        scaled,
                        0.0,
                        0.0,
                        result.derived_evidence_ids(),
                        format!(
                            "the pool's top lexical score, which divides this candidate's lexical score {score}"
                        ),
                    ),
                )
            }
            Some(k) => {
                let rank = self.lexical_scores.partition_point(|&other| other > score) + 1;
                let scaled = if score > 0.0 {
                    (k as f32 + 1.0) / (k as f32 + rank as f32)
                } else {
                    0.0
                };
                (
                    scaled,
                    format!(
                        "{TEXT_RELEVANCE_RATIONALE}: replaced by its lexical rank, (k + 1) / (k + rank) with k = {k} and rank {rank} (lexical score {score})"
                    ),
                    ScoreComponent::new(
                        "text_relevance_rank",
                        rank as f32,
                        scaled,
                        0.0,
                        0.0,
                        result.derived_evidence_ids(),
                        format!(
                            "this candidate's rank among the pool's lexical scores, scaled as (k + 1) / (k + rank) with k = {k}"
                        ),
                    ),
                )
            }
        };
        features.text_relevance = scaled;
        // The penalty is a share of the score it is summed against.
        features.path_quality_penalty = path_quality_penalty(&path, scaled);
        let mut retained = result
            .score_breakdown
            .iter()
            .filter(|component| LEXICAL_SCORE_PARTS.contains(&component.signal.as_str()))
            .map(|component| {
                ScoreComponent::new(
                    component.signal.clone(),
                    component.raw_value,
                    component.normalized_value,
                    0.0,
                    0.0,
                    component.evidence_ids.clone(),
                    format!(
                        "{}; recorded at weight 0: part of the lexical score {score}, which is summed only as a scaled text_relevance",
                        component.rationale
                    ),
                )
            })
            .collect::<Vec<_>>();
        retained.push(scale_component);
        FusionInput {
            features,
            text_rationale: text_rationale.into(),
            retained,
        }
    }
}

fn is_lexical_origin(result: &SearchResult) -> bool {
    result
        .score_breakdown
        .iter()
        .any(|component| LEXICAL_ORIGIN_SIGNALS.contains(&component.signal.as_str()))
}

fn apply_fusion(result: &mut SearchResult, options: &RankingOptions, input: FusionInput) {
    let FusionInput {
        features,
        text_rationale,
        retained,
    } = input;
    let weights = options.weights;
    let disabled = match options.mode {
        RankingMode::WithoutSignal(signal) => Some(signal),
        RankingMode::Baseline | RankingMode::Fusion => None,
    };
    let evidence_ids = result.derived_evidence_ids();
    result.score_breakdown = Vec::new();
    for spec in [
        SignalSpec {
            signal: RankingSignal::TextRelevance,
            name: "text_relevance",
            raw_value: features.text_relevance,
            weight: weights.text_relevance,
            evidence_ids: evidence_ids.clone(),
            rationale: &text_rationale,
        },
        SignalSpec {
            signal: RankingSignal::ExactReference,
            name: "exact_reference",
            raw_value: features.exact_reference,
            weight: weights.exact_reference,
            evidence_ids: evidence_ids.clone(),
            rationale: "exact symbol reference or symbol-name match",
        },
        SignalSpec {
            signal: RankingSignal::GraphProximity,
            name: "graph_proximity",
            raw_value: features.graph_proximity,
            weight: weights.graph_proximity,
            evidence_ids: evidence_ids.clone(),
            rationale: "dependency or impact graph signal when available",
        },
        SignalSpec {
            signal: RankingSignal::BoundaryFit,
            name: "boundary_fit",
            raw_value: features.boundary_fit,
            weight: weights.boundary_fit,
            evidence_ids: evidence_ids.clone(),
            rationale:
                "source-like paths and symbol-bounded chunks are better primary edit candidates",
        },
        SignalSpec {
            signal: RankingSignal::RuntimeCorroboration,
            name: "runtime_corroboration",
            raw_value: features.runtime_corroboration,
            weight: weights.runtime_corroboration,
            evidence_ids: evidence_ids.clone(),
            rationale: "runtime traces or incidents near the result when available",
        },
        SignalSpec {
            signal: RankingSignal::GitCochange,
            name: "git_cochange",
            raw_value: features.git_cochange,
            weight: weights.git_cochange,
            evidence_ids: evidence_ids.clone(),
            rationale: "historical co-change signal when available",
        },
        SignalSpec {
            signal: RankingSignal::GitCochange,
            name: "history_churn",
            raw_value: features.history_churn,
            weight: weights.git_cochange,
            evidence_ids: evidence_ids.clone(),
            rationale: "bounded churn and hotspot history signal",
        },
        SignalSpec {
            signal: RankingSignal::GitCochange,
            name: "ownership_risk",
            raw_value: features.ownership_risk,
            weight: weights.git_cochange,
            evidence_ids: evidence_ids.clone(),
            rationale: "bounded ownership dispersion risk from local history",
        },
        SignalSpec {
            signal: RankingSignal::GitCochange,
            name: "similar_change_overlap",
            raw_value: features.similar_change_overlap,
            weight: weights.git_cochange,
            evidence_ids: evidence_ids.clone(),
            rationale: "bounded similar-change and co-change overlap signal",
        },
        SignalSpec {
            signal: RankingSignal::GitCochange,
            name: "reviewer_affinity",
            raw_value: features.reviewer_affinity,
            weight: weights.git_cochange,
            evidence_ids: evidence_ids.clone(),
            rationale: "bounded reviewer affinity from local history",
        },
        SignalSpec {
            signal: RankingSignal::ValidationProximity,
            name: "validation_proximity",
            raw_value: features.validation_proximity,
            weight: weights.validation_proximity,
            evidence_ids: evidence_ids.clone(),
            rationale: "test and validation paths are useful supporting context",
        },
        SignalSpec {
            signal: RankingSignal::MemorySignal,
            name: "memory_signal",
            raw_value: features.memory_signal,
            weight: weights.memory_signal,
            evidence_ids: evidence_ids.clone(),
            rationale: "repo memory signal when available",
        },
        SignalSpec {
            signal: RankingSignal::SemanticSimilarity,
            name: "semantic_similarity",
            raw_value: features.semantic_similarity,
            weight: weights.semantic_similarity,
            evidence_ids: evidence_ids.clone(),
            rationale: "local semantic vector similarity signal when available",
        },
        SignalSpec {
            signal: RankingSignal::PathQuality,
            name: "path_quality",
            raw_value: features.path_quality_penalty,
            weight: weights.path_quality,
            evidence_ids,
            rationale: "vendor and generated paths are lower-quality edit targets",
        },
    ] {
        push_signal(result, disabled, spec);
    }
    for component in retained {
        result.add_score_component(component);
    }
    result.score = open_kioku_core::score_component_total(&result.score_breakdown);
    result.reconcile_score_breakdown();
}

fn push_signal(result: &mut SearchResult, disabled: Option<RankingSignal>, spec: SignalSpec<'_>) {
    if disabled == Some(spec.signal) || spec.raw_value.abs() <= 0.001 || spec.weight.abs() <= 0.001
    {
        return;
    }
    result.add_score_component(ScoreComponent::new(
        spec.name,
        spec.raw_value,
        spec.raw_value.clamp(-1.0, 1.0),
        spec.weight,
        spec.raw_value * spec.weight,
        spec.evidence_ids,
        spec.rationale,
    ));
}

fn path_quality_penalty(path: &str, score: f32) -> f32 {
    let mut penalty = 0.0;
    if path.contains("vendor") {
        penalty -= score * 0.65;
    }
    if path.contains("generated")
        || path.contains("_pb.rs")
        || path.contains(".pb.go")
        || path.contains("schema.json")
    {
        penalty -= score * 0.45;
    }
    penalty
}

fn component_signal_value(result: &SearchResult, names: &[&str]) -> Option<f32> {
    result
        .score_breakdown
        .iter()
        .filter(|component| names.iter().any(|name| component.signal == *name))
        .map(|component| component.raw_value.abs().max(component.contribution.abs()))
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
}

fn is_test_path(path: &str) -> bool {
    open_kioku_core::is_test_path(path)
}

/// How plausible this result is as an edit target for the query.
///
/// Two defects are fixed here. A third, larger one is deliberately left alone.
///
/// Fixed: test paths returned 0.0 outright, so for a `code_to_test` query -
/// where the gold answer *is* a test file - the correct result was barred from
/// the signal while unrelated source files collected the top tier. And the top
/// tier fired when any single query term matched any path term, so one generic
/// token promoted unrelated files in unrelated languages.
///
/// Not fixed: the tiers are 18.0 / 0.63 / 0.03 while every other signal ranges
/// 0..1, which makes the configured weight nearly meaningless. That is not this
/// function's bug. `text_relevance` is raw unnormalized BM25 (observed 10-45)
/// at weight 1.0, so a 0..1 signal cannot be heard at all and 18.0 is what it
/// costs to be audible. Rescaling here without normalizing text_relevance just
/// silences the signal. Tracked separately.
fn boundary_fit_score(result: &SearchResult, path: &str, query: Option<&str>) -> f32 {
    // Docs are never edit targets.
    if is_docs_path(path) {
        return 0.0;
    }
    // Tests are edit targets only when the task is about tests. Barring them
    // unconditionally broke `code_to_test` queries, where the gold answer *is*
    // a test file. Admitting them unconditionally - which is what I did first -
    // broke ordinary queries on real repositories: "quota enforcer" returned
    // four test helpers before the enforcer, because a large Java project has
    // far more test files than source files and they match the same terms.
    if is_test_path(&result.path.to_string_lossy())
        && !query.map(query_wants_tests).unwrap_or(false)
    {
        return 0.0;
    }
    if query
        .map(|query| exact_identity_match(result, query))
        .unwrap_or(false)
    {
        return 18.0;
    }
    if query.map(is_structured_identifier).unwrap_or(false) {
        return if result.symbol.is_some() { 0.63 } else { 0.03 };
    }
    if query
        .map(|query| query_matches_path_discriminatively(query, path))
        .unwrap_or(false)
    {
        return 18.0;
    }
    let Some(symbol) = &result.symbol else {
        return 0.03;
    };
    let Some(stem) = result
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    else {
        return 0.63;
    };
    if query
        .map(|query| query_matches_symbol_or_stem(query, &symbol.name, &stem))
        .unwrap_or(false)
    {
        18.0
    } else {
        0.63
    }
}

fn query_wants_tests(query: &str) -> bool {
    open_kioku_core::query_wants_tests(query)
}

fn is_docs_path(path: &str) -> bool {
    path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.contains("/docs/")
        || path.starts_with("docs/")
}

fn exact_identity_match(result: &SearchResult, query: &str) -> bool {
    let query = normalize_identifier(query);
    if query.is_empty() {
        return false;
    }
    let file_stem_matches = result
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .is_some_and(|stem| normalize_identifier(stem) == query);
    file_stem_matches
        || result.symbol.as_ref().is_some_and(|symbol| {
            normalize_identifier(&symbol.name) == query
                || normalize_identifier(&symbol.qualified_name) == query
        })
}

fn is_structured_identifier(query: &str) -> bool {
    !query.chars().any(char::is_whitespace)
        && (query.chars().any(|ch| ch.is_ascii_uppercase())
            || query
                .chars()
                .any(|ch| matches!(ch, '_' | '-' | ':' | '.' | '/' | '\\')))
}

/// True when the query and the path agree on something discriminative.
///
/// The previous rule fired on a single shared term, so `service` in a query
/// promoted `go/shipping/service.go`, `python/orders/service.py` and
/// `rust/src/cache/service.rs` equally, in three languages none of which the
/// query mentioned. Two independent terms, or one term that is not a generic
/// domain noun, is the weakest signal that actually distinguishes a file.
fn query_matches_path_discriminatively(query: &str, path: &str) -> bool {
    let query_terms = identifier_terms(query);
    let matched: Vec<String> = identifier_terms(path)
        .into_iter()
        .filter(|term| !is_structural_path_term(term))
        .filter(|path_term| {
            query_terms
                .iter()
                .any(|query_term| terms_match(query_term, path_term))
        })
        .collect();
    match matched.len() {
        0 => false,
        1 => !is_generic_domain_term(&matched[0]),
        _ => true,
    }
}

/// Nouns so common across a polyglot repository that agreement on one of them
/// alone says nothing about which file to edit.
///
/// Deliberately short. Words that *name* a component - `config`, `core`,
/// `context`, `store` - are discriminative in a repository that has a crate by
/// that name, and excluding them demotes the file the query was actually about.
/// This list is a heuristic patched onto a heuristic; the real fix is corpus
/// term frequency, which the ranker does not have at scoring time.
fn is_generic_domain_term(term: &str) -> bool {
    matches!(
        term,
        "service"
            | "services"
            | "handler"
            | "handlers"
            | "util"
            | "utils"
            | "helper"
            | "helpers"
            | "common"
            | "base"
            | "data"
            | "value"
    )
}

fn identifier_terms(value: &str) -> Vec<String> {
    normalize_identifier(value)
        .split_whitespace()
        .filter(|term| term.len() >= 3)
        .map(ToString::to_string)
        .collect()
}

fn terms_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    // A 6-character floor meant the query term `order` could not match the path
    // term `orders`, while it did match `order_import` - so a gold directory lost
    // to a legacy distractor on a plural.
    if singularize(left) == singularize(right) {
        return true;
    }
    (left.len() >= 4 && right.starts_with(left)) || (right.len() >= 4 && left.starts_with(right))
}

fn singularize(term: &str) -> &str {
    term.strip_suffix("es")
        .filter(|stem| stem.len() >= 3)
        .or_else(|| term.strip_suffix('s').filter(|stem| stem.len() >= 3))
        .unwrap_or(term)
}

fn is_structural_path_term(term: &str) -> bool {
    matches!(
        term,
        "crates" | "open" | "kioku" | "src" | "lib" | "main" | "mod" | "index"
    )
}

fn query_matches_symbol_or_stem(query: &str, symbol_name: &str, stem: &str) -> bool {
    let normalized_symbol = normalize_identifier(symbol_name);
    let normalized_stem = normalize_identifier(stem);
    query_identifiers(query)
        .iter()
        .any(|candidate| candidate == &normalized_symbol || candidate == &normalized_stem)
}

fn query_identifiers(query: &str) -> Vec<String> {
    let mut values = Vec::new();
    let normalized_query = normalize_identifier(query);
    if !normalized_query.is_empty() {
        values.push(normalized_query);
    }
    for token in query.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        let normalized = normalize_identifier(token);
        if normalized.len() >= 3 && !values.iter().any(|existing| existing == &normalized) {
            values.push(normalized);
        }
    }
    values
}

fn normalize_identifier(value: &str) -> String {
    let mut out = String::new();
    let mut previous_lower_or_digit = false;
    for ch in value.chars() {
        if ch == '_' || ch == '-' || ch == '/' || ch == '.' {
            out.push(' ');
            previous_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower_or_digit {
            out.push(' ');
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(' ');
        }
        previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_test_file_can_be_a_boundary_match_for_a_test_query() {
        // `code_to_test` queries have a test file as the gold answer. Barring
        // test paths from boundary_fit demoted the correct result while
        // unrelated source files collected the top tier.
        let gold = make_result("java/test/com/acme/auth/AuthServiceTest.java", 1.0);
        let options = RankingOptions {
            query: Some("tests covering AuthService issueToken invalid credentials".into()),
            ..RankingOptions::default()
        };
        let results = rerank_with_options(vec![gold], &options);
        let boundary = results[0]
            .score_breakdown
            .iter()
            .find(|c| c.signal == "boundary_fit")
            .expect("test files must still receive the signal");
        assert!(
            boundary.raw_value > 0.03,
            "a test file matching a test query must not sit at the bottom tier, got {}",
            boundary.raw_value
        );
    }

    #[test]
    fn one_generic_term_does_not_earn_the_top_boundary_tier() {
        // `service` alone promoted unrelated files in three languages the query
        // never mentioned.
        assert!(
            !super::query_matches_path_discriminatively(
                "tests covering AuthService issueToken",
                "go/shipping/service.go"
            ),
            "a single generic domain term must not be treated as agreement"
        );
        assert!(
            super::query_matches_path_discriminatively(
                "fix the shipping service quote handler",
                "go/shipping/service.go"
            ),
            "two independent matching terms are discriminative"
        );
    }

    #[test]
    fn a_plural_path_term_matches_its_singular_query_term() {
        // The 6-character prefix floor meant `order` missed `orders` but hit
        // `order_import`, so a gold directory lost to a legacy distractor.
        assert!(super::terms_match("order", "orders"));
        assert!(super::terms_match("orders", "order"));
        assert!(!super::terms_match("order", "ordinal"));
    }
    use super::RankingFeatures;

    // Regression: Tantivy embeds the user's query into its evidence prose
    // ("query variant `X` matched local index"). Scoring must never read that
    // back as evidence of anything.
    fn probe_result(evidence: &str, reason: &str) -> SearchResult {
        SearchResult {
            path: std::path::PathBuf::from("src/cache/store.rs"),
            line_range: None,
            snippet: String::new(),
            symbol: None,
            score: 14.5,
            match_reason: reason.to_string(),
            evidence: vec![evidence.to_string()],
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        }
    }

    #[test]
    fn a_query_containing_trace_does_not_manufacture_runtime_evidence() {
        let result = probe_result(
            "query variant `rust trace cache miss` matched local index",
            "lexical match",
        );
        let features = RankingFeatures::from_result(&result, Some("rust trace cache miss"));
        assert_eq!(
            features.runtime_corroboration, 0.0,
            "runtime corroboration must come from persisted runtime evidence, \
             never from the word `trace` appearing in the user's own query"
        );
    }

    #[test]
    fn ordinary_words_in_a_query_do_not_manufacture_graph_or_memory_evidence() {
        let result = probe_result(
            "query variant `where is the dependency graph built in memory` matched local index",
            "lexical match",
        );
        let features = RankingFeatures::from_result(&result, None);
        assert_eq!(
            features.graph_proximity, 0.0,
            "graph signal must be persisted"
        );
        assert_eq!(
            features.memory_signal, 0.0,
            "memory signal must be persisted"
        );
        assert_eq!(
            features.git_cochange, 0.0,
            "co-change signal must be persisted"
        );
    }

    #[test]
    fn a_persisted_runtime_component_is_still_honoured() {
        let mut result = probe_result(
            "BM25 lexical match from local Tantivy index",
            "lexical match",
        );
        result.score_breakdown = vec![ScoreComponent {
            signal: "runtime_corroboration".into(),
            raw_value: 0.42,
            normalized_value: 0.42,
            weight: 0.30,
            contribution: 0.126,
            evidence_ids: vec!["runtime:1".into()],
            rationale: "persisted runtime evidence".into(),
        }];
        let features = RankingFeatures::from_result(&result, None);
        assert_eq!(
            features.runtime_corroboration, 0.42,
            "a real persisted signal must still be read"
        );
    }
    use super::{
        rerank, rerank_baseline, rerank_with_options, rerank_without_signal, top_score_signals,
        RankingMode, RankingOptions, RankingSignal, RankingWeights, TextRelevanceScale,
    };
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, LineRange, ScoreComponent, SearchResult,
        Symbol, SymbolId, SymbolKind,
    };
    use std::path::{Path, PathBuf};

    fn make_result(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange::single(1)),
            snippet: "some code".into(),
            symbol: None,
            score,
            match_reason: "test".into(),
            evidence: vec!["test".into()],
            evidence_refs: Vec::new(),
            confidence: 0.6,
            score_breakdown: vec![ScoreComponent::single(
                "test_score",
                score,
                vec!["test".into()],
                "test fixture",
            )],
            exact_reference_provenance: None,
        }
    }

    #[test]
    fn vendor_files_score_lower() {
        let normal = make_result("src/lib.rs", 1.0);
        let vendor = make_result("vendor/dep/lib.rs", 1.0);
        let results = rerank(vec![normal, vendor]);
        assert!(
            results[0].path.to_string_lossy().contains("src"),
            "normal file should outscore vendor"
        );
    }

    #[test]
    fn generated_files_score_lower() {
        let normal = make_result("src/lib.rs", 1.0);
        let generated = make_result("src/generated_pb.rs", 1.0);
        let results = rerank(vec![normal, generated]);
        assert!(
            results[0].path == Path::new("src/lib.rs"),
            "Expected src/lib.rs to be first, got {:?}",
            results[0].path
        );
    }

    #[test]
    fn test_files_score_slightly_higher() {
        let normal = make_result("src/lib.rs", 1.0);
        let test = make_result("src/lib_test.rs", 1.0);
        let results = rerank(vec![normal, test]);
        let test_score = results
            .iter()
            .find(|r| r.path.to_string_lossy().contains("test"))
            .map(|r| r.score)
            .unwrap();
        assert!(test_score > 1.0, "test file should receive boost");
    }

    #[test]
    fn results_sorted_descending() {
        let low = make_result("src/a.rs", 0.3);
        let high = make_result("src/b.rs", 0.9);
        let results = rerank(vec![low, high]);
        assert!(results[0].score >= results[1].score);
    }

    #[test]
    fn fusion_records_dominant_signals() {
        let mut exact = make_result("src/a.rs", 1.0);
        exact.match_reason = "exact symbol reference via SCIP".into();
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        exact.evidence = vec!["exact reference from scip".into()];
        let results = rerank(vec![exact]);
        let signals = top_score_signals(&results[0], 3);
        assert!(signals
            .iter()
            .any(|signal| signal.contains("text_relevance")));
        assert!(signals
            .iter()
            .any(|signal| signal.contains("exact_reference")));
    }

    #[test]
    fn exact_reference_dominates_bounded_history_signal() {
        let mut exact = make_result("src/exact.rs", 0.1);
        exact.match_reason = "exact symbol reference via SCIP".into();
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        exact.evidence = vec!["exact reference from scip".into()];

        let mut historical = make_result("src/history.rs", 0.1);
        historical.evidence =
            vec!["history signal for `src/history.rs`: similar change overlap".into()];
        historical.score_breakdown = vec![ScoreComponent::adjustment(
            "similar_change_overlap",
            0.18,
            vec!["history-similar:abc".into()],
            "bounded similar-change overlap from persisted local history",
        )];

        let results = rerank(vec![historical, exact]);

        assert_eq!(results[0].path, Path::new("src/exact.rs"));
        let history = results
            .iter()
            .find(|result| result.path == Path::new("src/history.rs"))
            .unwrap();
        assert!(history
            .score_breakdown
            .iter()
            .any(|component| component.signal == "similar_change_overlap"));
    }

    #[test]
    fn ablation_removes_named_signal() {
        let mut exact = make_result("src/a.rs", 1.0);
        exact.match_reason = "exact symbol reference via SCIP".into();
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        let fused = rerank(vec![exact.clone()]);
        let ablated = rerank_without_signal(vec![exact], RankingSignal::ExactReference);
        assert!(fused[0].score > ablated[0].score);
    }

    #[test]
    fn baseline_preserves_original_order_by_score() {
        let low = make_result("src/a.rs", 0.3);
        let high = make_result("src/b.rs", 0.9);
        let results = rerank_baseline(vec![low, high]);
        assert_eq!(results[0].path, Path::new("src/b.rs"));
    }

    #[test]
    fn default_weights_are_documented_values() {
        let weights = RankingWeights::default();
        assert_eq!(weights.text_relevance, 1.0);
        assert_eq!(weights.validation_proximity, 1.0);
        assert_eq!(weights.graph_proximity, 0.35);
    }

    #[test]
    fn source_symbol_file_stem_can_beat_higher_scoring_test_context() {
        let mut source = make_result("src/DotPrefixValidator.java", 44.5);
        source.symbol = Some(Symbol {
            id: SymbolId::new("dot-prefix-validator"),
            name: "DotPrefixValidator".into(),
            qualified_name: "com.acme.validation.DotPrefixValidator".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("source"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        });
        let test = make_result("src/test/DotPrefixValidatorTests.java", 48.1);

        let options = RankingOptions {
            query: Some("DotPrefixValidator".into()),
            ..RankingOptions::default()
        };
        let results = rerank_with_options(vec![test, source], &options);

        assert_eq!(results[0].path, Path::new("src/DotPrefixValidator.java"));
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "boundary_fit" && component.raw_value >= 1.0));
    }

    #[test]
    fn exact_structured_identifier_outranks_a_higher_scoring_prefix_match() {
        let mut exact = make_result("src/DispatcherServlet.java", 32.0);
        exact.symbol = Some(Symbol {
            id: SymbolId::new("dispatcher-servlet"),
            name: "DispatcherServlet".into(),
            qualified_name: "org.springframework.web.DispatcherServlet".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("exact"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        });
        let mut prefix = make_result("src/Dispatcher.java", 42.0);
        prefix.symbol = Some(Symbol {
            id: SymbolId::new("dispatcher"),
            name: "Dispatcher".into(),
            qualified_name: "org.springframework.cglib.Dispatcher".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("prefix"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        });

        let results = rerank_with_options(
            vec![prefix, exact],
            &RankingOptions {
                query: Some("DispatcherServlet".into()),
                ..RankingOptions::default()
            },
        );

        assert_eq!(results[0].path, Path::new("src/DispatcherServlet.java"));
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "exact_reference"));
        assert!(!results[1]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "exact_reference"));
        assert_eq!(
            results[1]
                .score_breakdown
                .iter()
                .find(|component| component.signal == "boundary_fit")
                .map(|component| component.raw_value),
            Some(0.63)
        );
    }

    #[test]
    fn crate_path_anchor_can_beat_unrelated_higher_lexical_score() {
        let config = make_result("crates/open-kioku-config/src/lib.rs", 1.0);
        let unrelated = make_result("crates/open-kioku-storage/src/lib.rs", 4.0);
        let options = RankingOptions {
            query: Some("add history configuration defaults".into()),
            ..RankingOptions::default()
        };

        let results = rerank_with_options(vec![unrelated, config], &options);

        assert_eq!(
            results[0].path,
            Path::new("crates/open-kioku-config/src/lib.rs")
        );
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "boundary_fit" && component.raw_value > 1.0));
    }

    #[test]
    fn structural_path_terms_do_not_create_boundary_match() {
        let source = make_result("crates/open-kioku-config/src/lib.rs", 1.0);
        let options = RankingOptions {
            query: Some("change source library".into()),
            ..RankingOptions::default()
        };

        let results = rerank_with_options(vec![source], &options);
        let boundary_fit = results[0]
            .score_breakdown
            .iter()
            .find(|component| component.signal == "boundary_fit")
            .expect("source files retain the default boundary signal");

        assert_eq!(boundary_fit.raw_value, 0.03);
    }

    #[test]
    fn semantic_only_result_does_not_outrank_exact_reference() {
        let mut exact = make_result("src/exact.rs", 0.45);
        exact.match_reason = "exact symbol reference via SCIP".into();
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        exact.evidence = vec!["exact reference from scip".into()];

        let mut semantic = make_result("src/semantic.rs", 0.99);
        semantic.match_reason = "semantic vector match".into();
        semantic.evidence = vec!["semantic vector relationship".into()];
        semantic.score_breakdown = vec![ScoreComponent::single(
            "semantic_similarity",
            0.99,
            vec!["semantic".into()],
            "semantic-only fixture",
        )];

        let results = rerank(vec![semantic, exact]);

        assert_eq!(results[0].path, Path::new("src/exact.rs"));
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "exact_reference"));
        assert!(results[1]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "semantic_similarity"));
    }

    const SCALED: [TextRelevanceScale; 2] = [
        TextRelevanceScale::PoolMax,
        TextRelevanceScale::Rank { k: 10 },
    ];

    fn lexical_result(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange::single(1)),
            snippet: "some code".into(),
            symbol: None,
            score,
            match_reason: "tantivy hybrid lexical match".into(),
            evidence: vec!["BM25 lexical match from local Tantivy index".into()],
            evidence_refs: Vec::new(),
            confidence: 0.6,
            score_breakdown: vec![ScoreComponent::single(
                "bm25_relevance",
                score,
                vec!["lexical".into()],
                "BM25 score from local Tantivy index",
            )],
            exact_reference_provenance: None,
        }
    }

    fn component<'a>(result: &'a SearchResult, signal: &str) -> Option<&'a ScoreComponent> {
        result
            .score_breakdown
            .iter()
            .find(|component| component.signal == signal)
    }

    fn result_at<'a>(results: &'a [SearchResult], path: &str) -> &'a SearchResult {
        results
            .iter()
            .find(|result| result.path == Path::new(path))
            .expect("path is in the ranked pool")
    }

    fn ranked_paths(results: &[SearchResult]) -> Vec<PathBuf> {
        results.iter().map(|result| result.path.clone()).collect()
    }

    fn scaled_options(scale: TextRelevanceScale) -> RankingOptions {
        RankingOptions {
            text_relevance_scale: scale,
            ..RankingOptions::default()
        }
    }

    fn text_only_weights() -> RankingWeights {
        RankingWeights {
            text_relevance: 1.0,
            exact_reference: 0.0,
            graph_proximity: 0.0,
            boundary_fit: 0.0,
            runtime_corroboration: 0.0,
            git_cochange: 0.0,
            validation_proximity: 0.0,
            memory_signal: 0.0,
            path_quality: 0.0,
            semantic_similarity: 0.0,
        }
    }

    /// Thirty lexical candidates, the highest at 45.0 x `magnitude`, with persisted graph and
    /// runtime components, a test path and a vendor path, so default weights have signals to sum.
    fn signal_pool(magnitude: f32) -> Vec<SearchResult> {
        (0..30)
            .map(|index| {
                let path = match index {
                    3 => "vendor/dep/unit_03.rs".to_string(),
                    7 => "src/unit_07_test.rs".to_string(),
                    _ => format!("src/unit_{index:02}.rs"),
                };
                let mut result = lexical_result(&path, (45.0 - index as f32 * 1.25) * magnitude);
                if index % 4 == 1 {
                    result.score_breakdown.push(ScoreComponent::adjustment(
                        "graph_proximity",
                        0.8,
                        vec![format!("graph:{index}")],
                        "persisted graph proximity",
                    ));
                }
                if index % 5 == 2 {
                    result.score_breakdown.push(ScoreComponent::adjustment(
                        "runtime_corroboration",
                        0.5,
                        vec![format!("runtime:{index}")],
                        "persisted runtime evidence",
                    ));
                }
                result
            })
            .collect()
    }

    #[test]
    fn raw_scale_is_the_default_and_keeps_the_unscaled_breakdown() {
        assert_eq!(
            RankingOptions::default().text_relevance_scale,
            TextRelevanceScale::Raw
        );
        let results = rerank(vec![
            lexical_result("src/b.rs", 22.5),
            lexical_result("src/a.rs", 45.0),
        ]);
        let text = component(&results[0], "text_relevance").expect("text relevance is recorded");
        assert_eq!(text.raw_value, 45.0);
        assert_eq!(text.rationale, "BM25 or lexical score from indexed text");
        assert!(results
            .iter()
            .all(|result| component(result, "bm25_relevance").is_none()));
    }

    #[test]
    fn text_relevance_is_scaled_by_the_pool_maximum() {
        let results = rerank_with_options(
            vec![
                lexical_result("src/c.rs", 10.0),
                lexical_result("src/a.rs", 45.0),
                lexical_result("src/b.rs", 22.5),
            ],
            &scaled_options(TextRelevanceScale::PoolMax),
        );
        for (path, expected, raw) in [
            ("src/a.rs", 1.0, 45.0),
            ("src/b.rs", 0.5, 22.5),
            ("src/c.rs", 10.0 / 45.0, 10.0),
        ] {
            let result = result_at(&results, path);
            let text = component(result, "text_relevance").expect("text relevance is recorded");
            assert!(
                (text.raw_value - expected).abs() < 1e-6,
                "{path}: {}",
                text.raw_value
            );
            assert!(
                text.rationale.contains("/ 45)"),
                "the rationale must name the scale: {}",
                text.rationale
            );
            let bm25 = component(result, "bm25_relevance").expect("the raw BM25 component is kept");
            assert_eq!((bm25.raw_value, bm25.contribution), (raw, 0.0));
            let divisor = component(result, "text_relevance_pool_max")
                .expect("the divisor is recorded as a number, not only in prose");
            assert_eq!((divisor.raw_value, divisor.contribution), (45.0, 0.0));
            assert_eq!(divisor.normalized_value, text.raw_value);
        }
        assert_eq!(
            ranked_paths(&results),
            vec![
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.rs"),
                PathBuf::from("src/c.rs")
            ]
        );
    }

    #[test]
    fn rank_scale_gives_equal_lexical_scores_the_same_rank() {
        let results = rerank_with_options(
            vec![
                lexical_result("src/a.rs", 30.0),
                lexical_result("src/b.rs", 30.0),
                lexical_result("src/c.rs", 12.0),
            ],
            &scaled_options(TextRelevanceScale::Rank { k: 10 }),
        );
        let text =
            |path| component(result_at(&results, path), "text_relevance").map(|c| c.raw_value);
        assert_eq!(text("src/a.rs"), Some(1.0));
        assert_eq!(text("src/b.rs"), Some(1.0));
        // Two scores sit above it, so it is rank 3, not rank 2.
        assert_eq!(text("src/c.rs"), Some(11.0 / 13.0));
        let rank = component(result_at(&results, "src/c.rs"), "text_relevance_rank")
            .expect("the rank is recorded as a number, not only in prose");
        assert_eq!((rank.raw_value, rank.contribution), (3.0, 0.0));
    }

    #[test]
    fn scaled_text_relevance_reproduces_the_raw_order_when_every_other_weight_is_zero() {
        // Scrambled scores with a four-way tie at 20.5, a vendor path, a test path, and a
        // file stem the second query names exactly, so the identity tier is exercised too.
        let pool = || {
            (0..24)
                .map(|index| {
                    let path = match index {
                        5 => "vendor/dep/unit_05.rs".to_string(),
                        9 => "src/unit_09_test.rs".to_string(),
                        _ => format!("src/unit_{index:02}.rs"),
                    };
                    let score = if index % 8 == 0 {
                        20.5
                    } else {
                        3.0 + ((index * 7) % 24) as f32 * 1.75
                    };
                    lexical_result(&path, score)
                })
                .collect::<Vec<_>>()
        };
        for query in [None, Some("unit_03".to_string())] {
            let options = |scale| RankingOptions {
                weights: text_only_weights(),
                mode: RankingMode::Fusion,
                query: query.clone(),
                text_relevance_scale: scale,
            };
            let raw = rerank_with_options(pool(), &options(TextRelevanceScale::Raw));
            for scale in SCALED {
                let scaled = rerank_with_options(pool(), &options(scale));
                assert_eq!(
                    ranked_paths(&scaled),
                    ranked_paths(&raw),
                    "{scale:?} reordered the pool for query {query:?}"
                );
            }
        }
    }

    #[test]
    fn scaled_text_relevance_is_invariant_to_pool_depth_and_paging() {
        // The CLI fetches 100 candidates for `--limit 20` and 200 for `--limit 50`. A tail of
        // lower-scoring candidates stands in for the deeper fetch: the first two pages must keep
        // their paths and their fused scores bit for bit, which a min-max scale would not.
        for scale in SCALED {
            let options = scaled_options(scale);
            let shallow = rerank_with_options(signal_pool(1.0), &options);
            let mut deep_pool = signal_pool(1.0);
            deep_pool.extend((0..100).map(|index| {
                lexical_result(
                    &format!("src/tail/filler_{index:03}.rs"),
                    0.6 - index as f32 * 0.005,
                )
            }));
            let deep = rerank_with_options(deep_pool, &options);
            let page = |results: &[SearchResult]| {
                results
                    .iter()
                    .map(|result| (result.path.clone(), result.score.to_bits()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                page(&shallow[..10]),
                page(&deep[..10]),
                "{scale:?} moved the first page"
            );
            assert_eq!(
                page(&shallow[10..20]),
                page(&deep[10..20]),
                "{scale:?} moved the second page"
            );
        }
    }

    #[test]
    fn scaled_text_relevance_is_invariant_to_lexical_score_magnitude() {
        // BM25 magnitude moves with corpus size and query length; the scaled value must not.
        for scale in SCALED {
            let options = scaled_options(scale);
            let unit = rerank_with_options(signal_pool(1.0), &options);
            let magnified = rerank_with_options(signal_pool(3.7), &options);
            assert_eq!(ranked_paths(&unit), ranked_paths(&magnified), "{scale:?}");
            for (left, right) in unit.iter().zip(&magnified) {
                assert!(
                    (left.score - right.score).abs() < 1e-5,
                    "{scale:?} {}: {} against {}",
                    left.path.display(),
                    left.score,
                    right.score
                );
            }
        }
    }

    #[test]
    fn semantic_candidates_do_not_set_the_text_scale() {
        // A cosine of 0.99 above a lexical score of 0.5: had the semantic score set the scale,
        // the lexical hit would read 0.505 under PoolMax and 11/12 under Rank.
        let lexical = lexical_result("src/lexical.rs", 0.5);
        let mut semantic = make_result("src/semantic.rs", 0.99);
        semantic.match_reason = "semantic vector match".into();
        semantic.score_breakdown = vec![ScoreComponent::single(
            "semantic_similarity",
            0.99,
            vec!["semantic".into()],
            "semantic-only fixture",
        )];
        for scale in SCALED {
            let results = rerank_with_options(
                vec![semantic.clone(), lexical.clone()],
                &scaled_options(scale),
            );
            let lexical_text = component(result_at(&results, "src/lexical.rs"), "text_relevance")
                .expect("the lexical hit keeps its text relevance");
            assert_eq!(lexical_text.raw_value, 1.0, "{scale:?}");
            assert!(
                component(result_at(&results, "src/semantic.rs"), "text_relevance").is_none(),
                "{scale:?}: a semantic-only candidate carries no text relevance"
            );
        }
    }

    #[test]
    fn in_memory_lexical_fallback_scores_are_scaled_by_the_pool_maximum() {
        let fallback = |path: &str, score: f32| {
            let mut result = lexical_result(path, score);
            result.match_reason = "lexical substring match".into();
            result.score_breakdown = vec![ScoreComponent::single(
                "lexical_relevance",
                score,
                vec!["lexical".into()],
                "lexical phrase/token score adjusted for generated and vendor paths",
            )];
            result
        };
        let results = rerank_with_options(
            vec![fallback("src/low.rs", 2.0), fallback("src/high.rs", 8.0)],
            &scaled_options(TextRelevanceScale::PoolMax),
        );
        for (path, expected, raw) in [("src/high.rs", 1.0, 8.0), ("src/low.rs", 0.25, 2.0)] {
            let result = result_at(&results, path);
            assert_eq!(
                component(result, "text_relevance").map(|c| c.raw_value),
                Some(expected),
                "{path}"
            );
            let kept = component(result, "lexical_relevance")
                .expect("the fallback's own component is kept");
            assert_eq!((kept.raw_value, kept.contribution), (raw, 0.0));
        }
    }

    #[test]
    fn path_quality_penalty_is_proportional_to_scaled_text_relevance() {
        // Unscaled, the vendor penalty is -0.65 x 45 = -29.25, an exile tier rather than a
        // proportional penalty. Scaled, it cannot exceed 0.65 of a text relevance of at most 1.
        for scale in SCALED {
            let results = rerank_with_options(
                vec![
                    lexical_result("vendor/dep/lib.rs", 45.0),
                    lexical_result("src/lib.rs", 45.0),
                ],
                &scaled_options(scale),
            );
            assert_eq!(results[0].path, Path::new("src/lib.rs"), "{scale:?}");
            let penalty = component(result_at(&results, "vendor/dep/lib.rs"), "path_quality")
                .expect("the vendor path is still penalised");
            assert!(
                penalty.contribution < 0.0 && penalty.contribution >= -0.65,
                "{scale:?}: {}",
                penalty.contribution
            );
        }
    }

    #[test]
    fn baseline_mode_leaves_lexical_scores_unscaled_under_every_scale() {
        // The pack's lexical stream and the benchmark's `lexical` strategy rank in Baseline mode.
        let pool = || {
            vec![
                lexical_result("src/low.rs", 22.5),
                lexical_result("src/high.rs", 45.0),
            ]
        };
        let shape = |results: Vec<SearchResult>| {
            results
                .into_iter()
                .map(|result| (result.path, result.score.to_bits(), result.score_breakdown))
                .collect::<Vec<_>>()
        };
        let expected = shape(rerank_baseline(pool()));
        assert_eq!(
            expected
                .iter()
                .map(|(_, bits, _)| *bits)
                .collect::<Vec<_>>(),
            vec![45.0f32.to_bits(), 22.5f32.to_bits()]
        );
        for scale in [
            TextRelevanceScale::Raw,
            TextRelevanceScale::PoolMax,
            TextRelevanceScale::Rank { k: 10 },
        ] {
            let results = rerank_with_options(
                pool(),
                &RankingOptions {
                    mode: RankingMode::Baseline,
                    text_relevance_scale: scale,
                    ..RankingOptions::default()
                },
            );
            assert_eq!(shape(results), expected, "{scale:?}");
        }
    }

    #[test]
    fn candidates_outside_the_lexical_pool_take_no_scaled_text_relevance() {
        // A git co-change candidate carries no lexical component, and a semantic hit on a test
        // path is not semantic-only, because the path earns validation proximity. Scaled
        // against a lexical pool it never joined, the co-change candidate would take rank 101
        // and, with its history signal, outrank the weakest lexical hits under Rank.
        const CO_CHANGE: &str = "src/history_neighbour.rs";
        const SEMANTIC_TEST: &str = "tests/semantic_neighbour.rs";
        let mut co_change = make_result(CO_CHANGE, 0.2);
        co_change.match_reason = "historical git co-change candidate".into();
        co_change.score_breakdown = vec![ScoreComponent::single(
            "similar_change_overlap",
            0.18,
            vec!["history:1".into()],
            "candidate added from bounded historical similar-change evidence",
        )];
        let mut semantic_test = make_result(SEMANTIC_TEST, 0.99);
        semantic_test.match_reason = "semantic vector match".into();
        semantic_test.score_breakdown = vec![ScoreComponent::single(
            "semantic_similarity",
            0.99,
            vec!["semantic".into()],
            "semantic hit on a test path",
        )];
        for scale in SCALED {
            let mut pool = (0..100)
                .map(|index| {
                    lexical_result(
                        &format!("src/lexical_{index:03}.rs"),
                        30.0 - index as f32 * 0.05,
                    )
                })
                .collect::<Vec<_>>();
            pool.push(co_change.clone());
            pool.push(semantic_test.clone());
            let results = rerank_with_options(pool, &scaled_options(scale));
            for path in [CO_CHANGE, SEMANTIC_TEST] {
                let result = result_at(&results, path);
                assert!(
                    component(result, "text_relevance").is_none(),
                    "{scale:?} {path}: scaled against a lexical pool it never joined"
                );
                let excluded = component(result, "text_relevance_excluded")
                    .expect("the exclusion is recorded");
                assert_eq!(excluded.contribution, 0.0, "{scale:?} {path}");
            }
            assert!(
                component(result_at(&results, SEMANTIC_TEST), "validation_proximity").is_some(),
                "{scale:?}: the semantic fixture must not be semantic-only"
            );
            let position = |wanted: &str| {
                results
                    .iter()
                    .position(|result| result.path == Path::new(wanted))
                    .expect("path is ranked")
            };
            let weakest_lexical = results
                .iter()
                .rposition(|result| result.path.to_string_lossy().starts_with("src/lexical_"))
                .expect("lexical hits are ranked");
            assert!(
                position(CO_CHANGE) > weakest_lexical,
                "{scale:?}: the co-change candidate outranked a lexical hit on a borrowed rank"
            );
        }
    }

    #[test]
    fn a_single_candidate_pool_scales_its_text_relevance_to_one() {
        for scale in SCALED {
            let results = rerank_with_options(
                vec![lexical_result("src/only.rs", 17.25)],
                &scaled_options(scale),
            );
            assert_eq!(
                component(&results[0], "text_relevance").map(|c| c.raw_value),
                Some(1.0),
                "{scale:?}"
            );
            assert!(results[0].score.is_finite(), "{scale:?}");
        }
    }

    #[test]
    fn a_pool_whose_top_lexical_score_is_zero_stays_unscaled_and_finite() {
        for scale in SCALED {
            let results = rerank_with_options(
                vec![
                    lexical_result("src/b.rs", 0.0),
                    lexical_result("src/a.rs", 0.0),
                ],
                &scaled_options(scale),
            );
            assert_eq!(
                ranked_paths(&results),
                vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")],
                "{scale:?}"
            );
            for result in &results {
                assert!(result.score.is_finite(), "{scale:?}: {}", result.score);
                assert!(
                    result
                        .score_breakdown
                        .iter()
                        .all(|c| c.raw_value.is_finite()
                            && c.normalized_value.is_finite()
                            && c.contribution.is_finite()),
                    "{scale:?}: {:?}",
                    result.score_breakdown
                );
                assert!(
                    component(result, "text_relevance").is_none(),
                    "{scale:?}: a zero lexical score contributes no text relevance"
                );
                let unscaled = component(result, "text_relevance_unscaled")
                    .expect("why the pool was left unscaled is visible even at score 0");
                assert_eq!(unscaled.contribution, 0.0, "{scale:?}");
                assert!(
                    unscaled.rationale.contains("positive lexical score"),
                    "{scale:?}: {}",
                    unscaled.rationale
                );
            }
        }
    }

    #[test]
    fn exact_reference_prose_without_typed_provenance_scores_no_exact_reference_signal() {
        let has_exact_reference = |result: &SearchResult| {
            result
                .score_breakdown
                .iter()
                .any(|component| component.signal == "exact_reference")
        };
        let mut prose_only = make_result("src/caller.rs", 1.0);
        prose_only.match_reason = "exact symbol reference via SCIP".into();
        prose_only.evidence =
            vec!["exact reference to `issue_token` from `SCIP` occurrence data".into()];
        let results = rerank(vec![prose_only]);
        assert!(
            !has_exact_reference(&results[0]),
            "{:?}",
            results[0].score_breakdown
        );

        let mut typed = make_result("src/caller.rs", 1.0);
        typed.exact_reference_provenance = Some(EvidenceSourceType::TreeSitter);
        let results = rerank(vec![typed]);
        assert!(has_exact_reference(&results[0]));
    }
}
