fn ranking_ablation_signals() -> Vec<RankingSignal> {
    vec![
        RankingSignal::TextRelevance,
        RankingSignal::ExactReference,
        RankingSignal::GraphProximity,
        RankingSignal::BoundaryFit,
        RankingSignal::RuntimeCorroboration,
        RankingSignal::GitCochange,
        RankingSignal::ValidationProximity,
        RankingSignal::MemorySignal,
        RankingSignal::SemanticSimilarity,
        RankingSignal::PathQuality,
    ]
}

fn ranking_signal_name(signal: RankingSignal) -> &'static str {
    match signal {
        RankingSignal::TextRelevance => "text_relevance",
        RankingSignal::ExactReference => "exact_reference",
        RankingSignal::GraphProximity => "graph_proximity",
        RankingSignal::BoundaryFit => "boundary_fit",
        RankingSignal::RuntimeCorroboration => "runtime_corroboration",
        RankingSignal::GitCochange => "git_cochange",
        RankingSignal::ValidationProximity => "validation_proximity",
        RankingSignal::MemorySignal => "memory_signal",
        RankingSignal::SemanticSimilarity => "semantic_similarity",
        RankingSignal::PathQuality => "path_quality",
    }
}

