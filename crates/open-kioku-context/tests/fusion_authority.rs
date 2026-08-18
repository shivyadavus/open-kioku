use open_kioku_context::candidates::{
    fuse_candidate_streams, CandidateStream, FusionConfig, StreamCandidate, DEFAULT_RRF_K,
};
use open_kioku_core::{
    LineRange, RetrievalAuthority, RetrievalSourceKind, SearchResult,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn result(path: &str, score: f32) -> SearchResult {
    SearchResult {
        path: PathBuf::from(path),
        line_range: Some(LineRange { start: 1, end: 3 }),
        snippet: format!("fixture {path}"),
        symbol: None,
        score,
        match_reason: "fixture".into(),
        evidence: Vec::new(),
        evidence_refs: vec![format!("evidence:{path}")],
        confidence: 0.5,
        score_breakdown: Vec::new(),
    }
}

fn candidate(
    path: &str,
    score: f32,
    authority: RetrievalAuthority,
) -> StreamCandidate {
    StreamCandidate::from_result(result(path, score), authority, "adversarial fixture")
}

fn config(weights: &[(RetrievalSourceKind, f32)]) -> FusionConfig {
    FusionConfig {
        rrf_k: DEFAULT_RRF_K,
        source_weights: weights.iter().copied().collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn extreme_heuristic_weight_cannot_displace_authoritative_evidence() {
    let streams = vec![
        CandidateStream::success(
            RetrievalSourceKind::ExactSemantic,
            vec![candidate(
                "src/authoritative.rs",
                0.01,
                RetrievalAuthority::Authoritative,
            )],
        ),
        CandidateStream::success(
            RetrievalSourceKind::SemanticVector,
            vec![candidate(
                "src/heuristic.rs",
                1.0,
                RetrievalAuthority::Heuristic,
            )],
        ),
    ];
    let fused = fuse_candidate_streams(
        &streams,
        10,
        &config(&[
            (RetrievalSourceKind::ExactSemantic, 0.000_001),
            (RetrievalSourceKind::SemanticVector, 1_000_000.0),
        ]),
    );

    assert_eq!(fused.results[0].path, PathBuf::from("src/authoritative.rs"));
    assert_eq!(fused.diagnostics.traces[0].authority, RetrievalAuthority::Authoritative);
    assert_eq!(fused.diagnostics.traces[1].authority, RetrievalAuthority::Heuristic);
}

#[test]
fn source_agreement_only_reorders_candidates_with_equal_authority() {
    let streams = vec![
        CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![
                candidate("src/agreed.rs", 0.7, RetrievalAuthority::Heuristic),
                candidate("src/lexical_only.rs", 0.9, RetrievalAuthority::Heuristic),
            ],
        ),
        CandidateStream::success(
            RetrievalSourceKind::SemanticVector,
            vec![candidate(
                "src/agreed.rs",
                0.8,
                RetrievalAuthority::Heuristic,
            )],
        ),
    ];
    let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::unweighted());

    assert_eq!(fused.results[0].path, PathBuf::from("src/agreed.rs"));
    assert!(fused.diagnostics.traces[0].contributions.len() >= 2);
    assert!(fused
        .diagnostics
        .traces
        .iter()
        .all(|trace| trace.authority == RetrievalAuthority::Heuristic));
}

#[test]
fn fusion_never_upgrades_authority_from_numeric_score_or_stream_count() {
    let streams = vec![
        CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![candidate(
                "src/multi_signal.rs",
                0.9,
                RetrievalAuthority::Heuristic,
            )],
        ),
        CandidateStream::success(
            RetrievalSourceKind::SemanticVector,
            vec![candidate(
                "src/multi_signal.rs",
                0.9,
                RetrievalAuthority::Heuristic,
            )],
        ),
        CandidateStream::success(
            RetrievalSourceKind::GitHistory,
            vec![candidate(
                "src/multi_signal.rs",
                0.9,
                RetrievalAuthority::Corroborating,
            )],
        ),
    ];
    let fused = fuse_candidate_streams(&streams, 10, &FusionConfig::evidence_prior_weighted());

    assert_eq!(fused.diagnostics.traces.len(), 1);
    assert_eq!(
        fused.diagnostics.traces[0].authority,
        RetrievalAuthority::Corroborating
    );
    assert_eq!(fused.diagnostics.traces[0].contributions.len(), 3);
    assert!(!fused.diagnostics.traces[0]
        .contributions
        .iter()
        .any(|contribution| contribution.authority == RetrievalAuthority::Authoritative));
}

#[test]
fn disabled_weight_is_fail_visible_and_cannot_contribute() {
    let streams = vec![
        CandidateStream::success(
            RetrievalSourceKind::Lexical,
            vec![candidate(
                "src/lexical.rs",
                1.0,
                RetrievalAuthority::Heuristic,
            )],
        ),
        CandidateStream::success(
            RetrievalSourceKind::ExactSemantic,
            vec![candidate(
                "src/exact.rs",
                1.0,
                RetrievalAuthority::Authoritative,
            )],
        ),
    ];
    let fused = fuse_candidate_streams(
        &streams,
        10,
        &config(&[
            (RetrievalSourceKind::Lexical, 1.0),
            (RetrievalSourceKind::ExactSemantic, 0.0),
        ]),
    );

    assert_eq!(fused.results.len(), 1);
    assert_eq!(fused.results[0].path, PathBuf::from("src/lexical.rs"));
    assert!(fused
        .diagnostics
        .caveats
        .iter()
        .any(|caveat| caveat.contains("ExactSemantic candidate stream was disabled")));
}

#[test]
fn candidate_order_is_deterministic_when_authority_and_fused_score_tie() {
    let left = CandidateStream::success(
        RetrievalSourceKind::Lexical,
        vec![
            candidate("src/zeta.rs", 0.5, RetrievalAuthority::Heuristic),
            candidate("src/alpha.rs", 0.5, RetrievalAuthority::Heuristic),
        ],
    );
    let right = CandidateStream::success(
        RetrievalSourceKind::SemanticVector,
        vec![
            candidate("src/zeta.rs", 0.5, RetrievalAuthority::Heuristic),
            candidate("src/alpha.rs", 0.5, RetrievalAuthority::Heuristic),
        ],
    );

    let forward = fuse_candidate_streams(
        &[left.clone(), right.clone()],
        10,
        &FusionConfig::unweighted(),
    );
    let reversed = fuse_candidate_streams(&[right, left], 10, &FusionConfig::unweighted());

    let forward_paths = forward
        .results
        .iter()
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let reversed_paths = reversed
        .results
        .iter()
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(forward_paths, reversed_paths);
    assert_eq!(
        forward_paths,
        vec![PathBuf::from("src/zeta.rs"), PathBuf::from("src/alpha.rs")]
    );
}
