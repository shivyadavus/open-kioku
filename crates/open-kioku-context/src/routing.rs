use open_kioku_core::{RetrievalRoutingDiagnostics, RetrievalSourceKind, TaskFamily};

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRoutingDecision {
    pub family: TaskFamily,
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub policy: RetrievalPolicy,
}

impl TaskRoutingDecision {
    pub fn diagnostics(&self) -> RetrievalRoutingDiagnostics {
        RetrievalRoutingDiagnostics {
            task_family: self.family,
            confidence: self.confidence,
            reasons: self.reasons.clone(),
            enabled_sources: self.policy.enabled_sources.clone(),
            required_evidence: self.policy.required_evidence.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalPolicy {
    pub enabled_sources: Vec<RetrievalSourceKind>,
    pub required_evidence: Vec<RetrievalSourceKind>,
    /// Per-source candidate allocation expressed as a multiplier of the caller's requested
    /// primary-file limit. This changes retrieval breadth, never evidence authority or RRF weight.
    pub candidate_factors: Vec<(RetrievalSourceKind, usize)>,
    pub preferred_context_shape: &'static str,
    pub fusion_profile: &'static str,
    pub missing_required_evidence_is_blocker: bool,
}

impl RetrievalPolicy {
    pub fn allows(&self, source: RetrievalSourceKind) -> bool {
        self.enabled_sources.contains(&source)
    }

    pub fn candidate_cap(&self, source: RetrievalSourceKind, requested_limit: usize) -> usize {
        let factor = self
            .candidate_factors
            .iter()
            .find_map(|(candidate_source, factor)| {
                (*candidate_source == source).then_some(*factor)
            })
            .unwrap_or(1);
        requested_limit.saturating_mul(factor).clamp(1, 200)
    }

    pub fn request_limit(&self, requested_limit: usize) -> usize {
        self.enabled_sources
            .iter()
            .map(|source| self.candidate_cap(*source, requested_limit))
            .max()
            .unwrap_or(requested_limit.max(1))
            .clamp(1, 200)
    }
}

pub fn classify_task(task: &str) -> TaskRoutingDecision {
    let lower = task.to_ascii_lowercase();
    let has_doc = contains_any(
        &lower,
        &[
            "document",
            "documentation",
            "docs",
            "readme",
            "guide",
            "adr",
            "markdown",
        ],
    );
    let has_code = contains_any(
        &lower,
        &[
            "implement",
            "implementation",
            "source code",
            "function",
            "method",
            "class",
            "module",
            "behavior",
            ".rs",
            ".java",
            ".py",
            ".ts",
            ".tsx",
            ".go",
        ],
    );
    let has_test = contains_any(
        &lower,
        &[
            "test",
            "tests",
            "fixture",
            "coverage",
            "validation",
            "verify",
            "regression",
        ],
    );
    let has_trace = contains_any(
        &lower,
        &[
            "stack trace",
            "traceback",
            "exception",
            "runtime error",
            "crash",
            "panic",
            "incident",
            "failure log",
        ],
    );
    let has_review = contains_any(
        &lower,
        &[
            "review comment",
            "review feedback",
            "requested changes",
            "pull request comment",
            "pr comment",
        ],
    );
    let has_ripple = contains_any(
        &lower,
        &[
            "impact",
            "ripple",
            "callers",
            "callees",
            "dependent",
            "dependency",
            "public api",
            "contract",
            "boundary",
        ],
    );
    let has_issue = contains_any(
        &lower,
        &[
            "issue",
            "ticket",
            "bug",
            "feature",
            "request",
            "implement",
            "fix",
        ],
    );

    if has_doc && has_code {
        return decision(
            TaskFamily::MixedCodeDocs,
            0.90,
            vec!["task contains explicit documentation and implementation/code targets".into()],
        );
    }
    if has_doc {
        return decision(
            TaskFamily::Documentation,
            0.92,
            vec!["task explicitly targets documentation content".into()],
        );
    }

    let specialized = [has_trace, has_review, has_ripple, has_test]
        .into_iter()
        .filter(|matched| *matched)
        .count();
    if specialized > 1 {
        return decision(
            TaskFamily::General,
            0.45,
            vec![
                "multiple specialized task-family signals matched; using conservative general retrieval rather than silently choosing one"
                    .into(),
            ],
        );
    }
    if has_trace {
        return decision(
            TaskFamily::TraceToCode,
            0.92,
            vec!["task contains runtime failure or trace language".into()],
        );
    }
    if has_review {
        return decision(
            TaskFamily::CommentToContext,
            0.92,
            vec!["task explicitly references review feedback/comment context".into()],
        );
    }
    if has_ripple {
        return decision(
            TaskFamily::EditToRipple,
            0.88,
            vec!["task asks for dependency, impact, contract, or boundary context".into()],
        );
    }
    if has_test {
        return decision(
            TaskFamily::CodeToTest,
            0.86,
            vec![
                "task explicitly targets tests, validation, coverage, or regression evidence"
                    .into(),
            ],
        );
    }
    if has_issue {
        return decision(
            TaskFamily::IssueToCode,
            0.78,
            vec![
                "task uses issue/change implementation language without a more specific family"
                    .into(),
            ],
        );
    }

    decision(
        TaskFamily::General,
        0.50,
        vec![
            "no deterministic task-family rule matched; using conservative general retrieval"
                .into(),
        ],
    )
}

fn decision(family: TaskFamily, confidence: f32, reasons: Vec<String>) -> TaskRoutingDecision {
    TaskRoutingDecision {
        family,
        confidence,
        reasons,
        policy: policy_for(family),
    }
}

fn policy_for(family: TaskFamily) -> RetrievalPolicy {
    use RetrievalSourceKind as S;
    match family {
        TaskFamily::Documentation => RetrievalPolicy {
            enabled_sources: vec![S::Document, S::ExactSemantic],
            required_evidence: vec![S::Document],
            candidate_factors: vec![(S::Document, 6), (S::ExactSemantic, 2)],
            preferred_context_shape: "document_sections_with_exact_code_anchors",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: true,
        },
        TaskFamily::MixedCodeDocs => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Document, S::Lexical],
            candidate_factors: vec![
                (S::Document, 4),
                (S::Lexical, 3),
                (S::ExactSemantic, 3),
                (S::Graph, 2),
                (S::SemanticVector, 2),
                (S::Validation, 2),
                (S::GitHistory, 2),
                (S::Runtime, 2),
            ],
            preferred_context_shape: "implementation_tests_and_document_sections",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: true,
        },
        TaskFamily::CodeToTest => RetrievalPolicy {
            enabled_sources: vec![
                S::Lexical,
                S::ExactSemantic,
                S::Graph,
                S::Validation,
                S::GitHistory,
                S::Runtime,
            ],
            required_evidence: vec![S::Validation],
            candidate_factors: vec![
                (S::Validation, 5),
                (S::ExactSemantic, 3),
                (S::Graph, 3),
                (S::GitHistory, 3),
                (S::Lexical, 2),
                (S::Runtime, 2),
            ],
            preferred_context_shape: "tests_fixtures_validation_and_changed_code",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: true,
        },
        TaskFamily::TraceToCode => RetrievalPolicy {
            enabled_sources: vec![
                S::Lexical,
                S::ExactSemantic,
                S::Graph,
                S::Validation,
                S::GitHistory,
                S::Runtime,
            ],
            required_evidence: vec![S::Runtime],
            candidate_factors: vec![
                (S::Runtime, 5),
                (S::Graph, 4),
                (S::ExactSemantic, 3),
                (S::Validation, 2),
                (S::GitHistory, 2),
                (S::Lexical, 2),
            ],
            preferred_context_shape: "runtime_failure_call_path_and_implementation",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: true,
        },
        TaskFamily::CommentToContext => RetrievalPolicy {
            enabled_sources: vec![
                S::Lexical,
                S::Document,
                S::ExactSemantic,
                S::Graph,
                S::Validation,
                S::GitHistory,
            ],
            required_evidence: vec![S::Lexical],
            candidate_factors: vec![
                (S::Lexical, 4),
                (S::ExactSemantic, 3),
                (S::Graph, 3),
                (S::Document, 2),
                (S::Validation, 2),
                (S::GitHistory, 2),
            ],
            preferred_context_shape: "review_anchor_changed_hunk_and_references",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: false,
        },
        TaskFamily::EditToRipple => RetrievalPolicy {
            enabled_sources: vec![
                S::Lexical,
                S::ExactSemantic,
                S::Graph,
                S::Validation,
                S::GitHistory,
                S::Runtime,
            ],
            required_evidence: vec![S::ExactSemantic, S::Graph],
            candidate_factors: vec![
                (S::ExactSemantic, 5),
                (S::Graph, 5),
                (S::Validation, 3),
                (S::GitHistory, 3),
                (S::Lexical, 2),
                (S::Runtime, 2),
            ],
            preferred_context_shape: "callers_callees_contracts_tests_and_boundaries",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: true,
        },
        TaskFamily::IssueToCode => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Lexical],
            candidate_factors: all_sources().into_iter().map(|source| (source, 4)).collect(),
            preferred_context_shape: "implementation_boundaries_and_tests",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: false,
        },
        TaskFamily::General => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: Vec::new(),
            candidate_factors: all_sources().into_iter().map(|source| (source, 4)).collect(),
            preferred_context_shape: "diverse_general_context",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: false,
        },
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn all_sources() -> [RetrievalSourceKind; 8] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_to_test_allocates_more_candidates_to_validation_without_document_noise() {
        let route = classify_task("find regression tests and validation for changed parser code");
        assert_eq!(route.family, TaskFamily::CodeToTest);
        assert!(route.policy.allows(RetrievalSourceKind::Validation));
        assert!(!route.policy.allows(RetrievalSourceKind::Document));
        assert_eq!(
            route.policy.required_evidence,
            vec![RetrievalSourceKind::Validation]
        );
        assert!(
            route.policy.candidate_cap(RetrievalSourceKind::Validation, 10)
                > route.policy.candidate_cap(RetrievalSourceKind::Lexical, 10)
        );
    }

    #[test]
    fn edit_to_ripple_allocates_more_candidates_to_exact_and_graph_evidence() {
        let route = classify_task(
            "show ripple across callers, callees, public API boundary and contract relationships",
        );
        assert_eq!(route.family, TaskFamily::EditToRipple);
        assert_eq!(
            route.policy.required_evidence,
            vec![
                RetrievalSourceKind::ExactSemantic,
                RetrievalSourceKind::Graph
            ]
        );
        let lexical = route.policy.candidate_cap(RetrievalSourceKind::Lexical, 10);
        assert!(route.policy.candidate_cap(RetrievalSourceKind::ExactSemantic, 10) > lexical);
        assert!(route.policy.candidate_cap(RetrievalSourceKind::Graph, 10) > lexical);
    }

    #[test]
    fn documentation_only_reserves_document_stream_while_mixed_keeps_both_domains() {
        let docs = classify_task("change documentation in the contributor guide");
        assert_eq!(docs.family, TaskFamily::Documentation);
        assert!(docs.policy.allows(RetrievalSourceKind::Document));
        assert!(!docs.policy.allows(RetrievalSourceKind::Lexical));
        assert!(
            docs.policy.candidate_cap(RetrievalSourceKind::Document, 10)
                > docs.policy.candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
        );

        let api_docs = classify_task("update the API guide with authentication examples");
        assert_eq!(api_docs.family, TaskFamily::Documentation);

        let mixed = classify_task("change retry behavior in the method and update documentation");
        assert_eq!(mixed.family, TaskFamily::MixedCodeDocs);
        assert!(mixed.policy.allows(RetrievalSourceKind::Document));
        assert!(mixed.policy.allows(RetrievalSourceKind::Lexical));
    }

    #[test]
    fn ambiguous_specialized_task_falls_back_to_conservative_general_policy() {
        let route = classify_task("fix panic and add regression tests for dependency impact");
        assert_eq!(route.family, TaskFamily::General);
        assert!(route.reasons.iter().any(|reason| reason.contains("multiple")));
        for source in all_sources() {
            assert!(route.policy.allows(source));
        }
        assert!(route.confidence < 0.5);
    }

    #[test]
    fn unknown_task_uses_conservative_general_policy() {
        let route = classify_task("understand the repository area around frobnication");
        assert_eq!(route.family, TaskFamily::General);
        for source in all_sources() {
            assert!(route.policy.allows(source));
        }
        assert!(route.confidence <= 0.5);
    }

    #[test]
    fn routing_policy_does_not_encode_unmeasured_fusion_weights() {
        let route = classify_task("trace runtime error to implementation");
        assert_eq!(route.family, TaskFamily::TraceToCode);
        assert_eq!(route.policy.fusion_profile, "existing_repository_rrf");
        assert!(route.policy.allows(RetrievalSourceKind::Runtime));
        assert!(route.policy.required_evidence.contains(&RetrievalSourceKind::Runtime));
    }
}
