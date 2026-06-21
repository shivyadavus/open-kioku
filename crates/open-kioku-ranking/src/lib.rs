use open_kioku_core::{ScoreComponent, SearchResult};

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

#[derive(Debug, Clone, PartialEq)]
pub struct RankingOptions {
    pub weights: RankingWeights,
    pub mode: RankingMode,
    pub query: Option<String>,
}

impl Default for RankingOptions {
    fn default() -> Self {
        Self {
            weights: RankingWeights::default(),
            mode: RankingMode::Fusion,
            query: None,
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
        let path = result.path.to_string_lossy().to_ascii_lowercase();
        let text = searchable_result_text(result);
        let evidence = result.evidence.join(" ").to_ascii_lowercase();
        let reason = result.match_reason.to_ascii_lowercase();
        let exact_reference = if reason.contains("exact symbol reference")
            || evidence.contains("exact reference")
            || evidence.contains("scip")
        {
            0.35
        } else {
            0.0
        };
        let graph_proximity = if evidence.contains("graph")
            || evidence.contains("dependency")
            || evidence.contains("direct impact")
        {
            0.12
        } else {
            0.0
        };
        let boundary_fit = boundary_fit_score(result, &path, query);
        let runtime_corroboration = if evidence.contains("runtime")
            || evidence.contains("trace")
            || evidence.contains("incident")
        {
            0.18
        } else {
            0.0
        };
        let git_cochange = if evidence.contains("co-change")
            || evidence.contains("cochange")
            || evidence.contains("git history")
        {
            0.12
        } else {
            0.0
        };
        let validation_proximity = if is_test_path(&path) { 0.05 } else { 0.0 };
        let memory_signal = if evidence.contains("memory") || reason.contains("memory") {
            0.08
        } else {
            0.0
        };
        let semantic_similarity = result
            .score_breakdown
            .iter()
            .find(|component| {
                component.signal == "semantic_similarity"
                    || component.signal == "local_semantic_similarity"
            })
            .map(|component| component.raw_value)
            .or_else(|| {
                if reason.contains("semantic") || evidence.contains("semantic vector") {
                    Some(result.score.clamp(0.0, 1.0))
                } else {
                    None
                }
            })
            .unwrap_or(0.0);
        let semantic_only = semantic_similarity > 0.0
            && exact_reference <= 0.0
            && graph_proximity <= 0.0
            && runtime_corroboration <= 0.0
            && git_cochange <= 0.0
            && validation_proximity <= 0.0;
        let text_relevance = if semantic_only { 0.0 } else { result.score };
        let path_quality_penalty = path_quality_penalty(&path, result.score);
        let symbol_name_hit = result
            .symbol
            .as_ref()
            .filter(|symbol| text.contains(&symbol.name.to_ascii_lowercase()))
            .map(|_| 0.15)
            .unwrap_or_default();

        Self {
            text_relevance,
            exact_reference: exact_reference + symbol_name_hit,
            graph_proximity,
            boundary_fit,
            runtime_corroboration,
            git_cochange,
            validation_proximity,
            memory_signal,
            path_quality_penalty,
            semantic_similarity,
        }
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
            for result in &mut results {
                apply_fusion(result, options);
            }
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
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

fn apply_fusion(result: &mut SearchResult, options: &RankingOptions) {
    let features = RankingFeatures::from_result(result, options.query.as_deref());
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
            rationale: "BM25 or lexical score from indexed text",
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
            rationale: "source-like paths are better primary edit candidates",
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

fn searchable_result_text(result: &SearchResult) -> String {
    format!(
        "{} {} {} {}",
        result.path.display(),
        result.snippet,
        result
            .symbol
            .as_ref()
            .map(|symbol| symbol.qualified_name.as_str())
            .unwrap_or_default(),
        result
            .symbol
            .as_ref()
            .map(|symbol| symbol.name.as_str())
            .unwrap_or_default()
    )
    .to_ascii_lowercase()
}

fn is_test_path(path: &str) -> bool {
    path.contains("test") || path.contains("/spec/") || path.ends_with("_test.rs")
}

fn is_source_path(path: &str) -> bool {
    !is_test_path(path)
        && !path.ends_with(".md")
        && !path.ends_with(".mdx")
        && !path.contains("/docs/")
        && !path.starts_with("docs/")
}

fn boundary_fit_score(result: &SearchResult, path: &str, query: Option<&str>) -> f32 {
    if !is_source_path(path) {
        return 0.0;
    }
    if query
        .map(|query| query_matches_path(query, path))
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
        return 0.03;
    };
    if query
        .map(|query| query_matches_symbol_or_stem(query, &symbol.name, &stem))
        .unwrap_or(false)
    {
        18.0
    } else {
        0.03
    }
}

fn query_matches_path(query: &str, path: &str) -> bool {
    let query_terms = identifier_terms(query);
    identifier_terms(path)
        .into_iter()
        .filter(|term| !is_structural_path_term(term))
        .any(|path_term| {
            query_terms
                .iter()
                .any(|query_term| terms_match(query_term, &path_term))
        })
}

fn identifier_terms(value: &str) -> Vec<String> {
    normalize_identifier(value)
        .split_whitespace()
        .filter(|term| term.len() >= 3)
        .map(ToString::to_string)
        .collect()
}

fn terms_match(left: &str, right: &str) -> bool {
    left == right
        || (left.len() >= 6 && right.starts_with(left))
        || (right.len() >= 6 && left.starts_with(right))
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
    use super::{
        rerank, rerank_baseline, rerank_with_options, rerank_without_signal, top_score_signals,
        RankingOptions, RankingSignal, RankingWeights,
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
    fn ablation_removes_named_signal() {
        let mut exact = make_result("src/a.rs", 1.0);
        exact.match_reason = "exact symbol reference via SCIP".into();
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
            qualified_name: "org.elasticsearch.validation.DotPrefixValidator".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("source"),
            range: Some(LineRange::single(1)),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
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
            .any(|component| component.signal == "boundary_fit" && component.raw_value > 1.0));
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
}
