use open_kioku_core::{QueryShape, RetrievalRoutingDiagnostics, RetrievalSourceKind, TaskFamily};

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRoutingDecision {
    pub family: TaskFamily,
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub query_shape: QueryShape,
    pub query_shape_confidence: f32,
    pub query_shape_signals: Vec<String>,
    pub query_shape_ambiguities: Vec<String>,
    pub query_shape_fallback_reason: Option<String>,
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
            query_shape: self.query_shape,
            query_shape_confidence: self.query_shape_confidence,
            query_shape_signals: self.query_shape_signals.clone(),
            query_shape_ambiguities: self.query_shape_ambiguities.clone(),
            query_shape_fallback_reason: self.query_shape_fallback_reason.clone(),
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
            .find_map(|(candidate_source, factor)| (*candidate_source == source).then_some(*factor))
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
    // Ripple routing is safety-critical because missing exact/graph evidence blocks retrieval.
    // Require explicit relationship or blast-radius intent rather than generic domain words such
    // as `impact` or `boundary`, which also occur in ordinary implementation tasks.
    let has_ripple = contains_any(
        &lower,
        &[
            "ripple",
            "callers",
            "callees",
            "dependent",
            "dependency",
            "public api",
            "blast radius",
            "impact radius",
            "affected callers",
            "affected callees",
            "affected dependents",
            "what breaks",
            "what would break",
            "what is impacted",
            "what gets impacted",
            "downstream impact",
            "upstream impact",
            "cross-boundary",
            "cross boundary",
        ],
    ) || contains_any(
        &lower,
        &["impact of ", "impact from ", "impact if ", "impact when "],
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
            task,
            TaskFamily::MixedCodeDocs,
            0.90,
            vec!["task contains explicit documentation and implementation/code targets".into()],
        );
    }
    if has_doc {
        return decision(
            task,
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
        return decision(task,
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
            task,
            TaskFamily::TraceToCode,
            0.92,
            vec!["task contains runtime failure or trace language".into()],
        );
    }
    if has_review {
        return decision(
            task,
            TaskFamily::CommentToContext,
            0.92,
            vec!["task explicitly references review feedback/comment context".into()],
        );
    }
    if has_ripple {
        return decision(task,
            TaskFamily::EditToRipple,
            0.88,
            vec![
                "task explicitly asks for dependency, ripple, caller/callee, or blast-radius context"
                    .into(),
            ],
        );
    }
    if has_test {
        return decision(
            task,
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
            task,
            TaskFamily::IssueToCode,
            0.78,
            vec![
                "task uses issue/change implementation language without a more specific family"
                    .into(),
            ],
        );
    }

    decision(
        task,
        TaskFamily::General,
        0.50,
        vec![
            "no deterministic task-family rule matched; using conservative general retrieval"
                .into(),
        ],
    )
}

fn decision(
    task: &str,
    family: TaskFamily,
    confidence: f32,
    reasons: Vec<String>,
) -> TaskRoutingDecision {
    let query = classify_query_shape(task);
    TaskRoutingDecision {
        family,
        confidence,
        reasons,
        query_shape: query.shape,
        query_shape_confidence: query.confidence,
        query_shape_signals: query.signals,
        query_shape_ambiguities: query.ambiguities,
        query_shape_fallback_reason: query.fallback_reason,
        policy: policy_for(family, query.shape),
    }
}

fn policy_for(family: TaskFamily, query_shape: QueryShape) -> RetrievalPolicy {
    use RetrievalSourceKind as S;
    let mut policy = match family {
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
            candidate_factors: all_sources()
                .into_iter()
                .map(|source| (source, 4))
                .collect(),
            preferred_context_shape: "implementation_boundaries_and_tests",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: false,
        },
        TaskFamily::General => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: Vec::new(),
            candidate_factors: all_sources()
                .into_iter()
                .map(|source| (source, 4))
                .collect(),
            preferred_context_shape: "diverse_general_context",
            fusion_profile: "existing_repository_rrf",
            missing_required_evidence_is_blocker: false,
        },
    };
    apply_query_shape(&mut policy, query_shape);
    policy
}

#[derive(Debug, Clone, PartialEq)]
struct QueryShapeDecision {
    shape: QueryShape,
    confidence: f32,
    signals: Vec<String>,
    ambiguities: Vec<String>,
    fallback_reason: Option<String>,
}

fn classify_query_shape(query: &str) -> QueryShapeDecision {
    let trimmed = query.trim();
    let lower = trimmed.to_ascii_lowercase();
    let mut structured = Vec::<(QueryShape, String)>::new();

    if is_error_trace_query(&lower) {
        structured.push((
            QueryShape::ErrorTrace,
            "query contains stack-trace/runtime-error structure".into(),
        ));
    }
    if contains_path_reference(trimmed) {
        structured.push((
            QueryShape::PathReference,
            "query contains a repository path or source-file reference".into(),
        ));
    }
    if is_qualified_symbol_query(trimmed) {
        structured.push((
            QueryShape::QualifiedSymbol,
            "query contains a qualified symbol/member expression".into(),
        ));
    } else if is_exact_identifier_query(trimmed) {
        structured.push((
            QueryShape::ExactIdentifier,
            "query is a single identifier-shaped token".into(),
        ));
    } else if contains_qualified_symbol_reference(trimmed) {
        structured.push((
            QueryShape::QualifiedSymbol,
            "natural-language query contains a qualified symbol/member reference".into(),
        ));
    } else if contains_named_identifier_reference(trimmed) {
        structured.push((
            QueryShape::ExactIdentifier,
            "natural-language query contains an identifier-shaped code reference".into(),
        ));
    }
    if is_api_resource_query(&lower) {
        structured.push((
            QueryShape::ApiResource,
            "query contains API/route/config/resource structure".into(),
        ));
    }

    structured.sort_by_key(|(shape, _)| query_shape_priority(*shape));
    structured.dedup_by(|left, right| left.0 == right.0);
    let natural_language = natural_language_token_count(trimmed) >= 3;

    if structured
        .iter()
        .any(|(shape, _)| *shape == QueryShape::ErrorTrace)
    {
        let signals = structured
            .iter()
            .map(|(_, signal)| signal.clone())
            .collect::<Vec<_>>();
        let non_trace = structured
            .iter()
            .filter(|(shape, _)| *shape != QueryShape::ErrorTrace)
            .map(|(shape, _)| format!("{shape:?}").to_ascii_lowercase())
            .collect::<Vec<_>>();
        let ambiguities = if non_trace.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "error trace also contains structured signals: {}",
                non_trace.join(", ")
            )]
        };
        return QueryShapeDecision {
            shape: QueryShape::ErrorTrace,
            confidence: if ambiguities.is_empty() { 0.96 } else { 0.90 },
            signals,
            ambiguities,
            fallback_reason: None,
        };
    }

    if structured.len() > 1
        || (structured.len() == 1 && natural_language && !single_structured_query(trimmed))
    {
        let signals = structured
            .iter()
            .map(|(_, signal)| signal.clone())
            .collect::<Vec<_>>();
        let ambiguities = if structured.len() > 1 {
            vec![format!(
                "multiple structured query signals matched: {}",
                structured
                    .iter()
                    .map(|(shape, _)| format!("{shape:?}").to_ascii_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            )]
        } else {
            Vec::new()
        };
        return QueryShapeDecision {
            shape: QueryShape::MixedStructuredNaturalLanguage,
            confidence: if ambiguities.is_empty() { 0.84 } else { 0.72 },
            signals,
            ambiguities,
            fallback_reason: None,
        };
    }

    if let Some((shape, signal)) = structured.into_iter().next() {
        return QueryShapeDecision {
            shape,
            confidence: 0.94,
            signals: vec![signal],
            ambiguities: Vec::new(),
            fallback_reason: None,
        };
    }

    if natural_language {
        return QueryShapeDecision {
            shape: QueryShape::Conceptual,
            confidence: 0.82,
            signals: vec!["query is unstructured natural-language concept text".into()],
            ambiguities: Vec::new(),
            fallback_reason: None,
        };
    }

    QueryShapeDecision {
        shape: QueryShape::Unknown,
        confidence: 0.40,
        signals: Vec::new(),
        ambiguities: Vec::new(),
        fallback_reason: Some(
            "no deterministic query-shape rule matched; preserving the task-family policy".into(),
        ),
    }
}

fn apply_query_shape(policy: &mut RetrievalPolicy, shape: QueryShape) {
    use RetrievalSourceKind as S;
    let deltas: &[(S, usize)] = match shape {
        QueryShape::ExactIdentifier | QueryShape::QualifiedSymbol => {
            &[(S::ExactSemantic, 2), (S::Lexical, 1), (S::Graph, 1)]
        }
        QueryShape::PathReference => &[
            (S::Lexical, 2),
            (S::ExactSemantic, 1),
            (S::SemanticVector, 1),
        ],
        QueryShape::ErrorTrace => &[
            (S::Runtime, 2),
            (S::ExactSemantic, 1),
            (S::Graph, 1),
            (S::Lexical, 1),
        ],
        QueryShape::ApiResource => &[
            (S::ExactSemantic, 1),
            (S::Lexical, 1),
            (S::Graph, 1),
            (S::Runtime, 1),
        ],
        QueryShape::Conceptual => &[
            (S::SemanticVector, 2),
            (S::Lexical, 1),
            (S::Graph, 1),
            (S::Document, 1),
        ],
        QueryShape::MixedStructuredNaturalLanguage => &[
            (S::ExactSemantic, 1),
            (S::Lexical, 1),
            (S::SemanticVector, 1),
            (S::Graph, 1),
            (S::Document, 1),
            (S::Runtime, 1),
        ],
        QueryShape::Unknown => &[],
    };

    let family_max = policy
        .candidate_factors
        .iter()
        .map(|(_, factor)| *factor)
        .max()
        .unwrap_or_default();
    let flat_family = policy
        .candidate_factors
        .iter()
        .all(|(_, factor)| *factor == family_max);

    for (source, delta) in deltas {
        if !policy.allows(*source) {
            continue;
        }
        if let Some((_, factor)) = policy
            .candidate_factors
            .iter_mut()
            .find(|(candidate_source, _)| candidate_source == source)
        {
            let original = *factor;
            let refined = original.saturating_add(*delta);
            *factor = if flat_family {
                refined
            } else if original == family_max {
                original
            } else {
                refined.min(family_max.saturating_sub(1).max(original))
            };
        }
    }
}

fn query_shape_priority(shape: QueryShape) -> u8 {
    match shape {
        QueryShape::ErrorTrace => 0,
        QueryShape::PathReference => 1,
        QueryShape::QualifiedSymbol => 2,
        QueryShape::ExactIdentifier => 3,
        QueryShape::ApiResource => 4,
        QueryShape::Conceptual => 5,
        QueryShape::MixedStructuredNaturalLanguage => 6,
        QueryShape::Unknown => 7,
    }
}

fn single_structured_query(query: &str) -> bool {
    !query.chars().any(char::is_whitespace)
}

fn is_exact_identifier_query(query: &str) -> bool {
    !query.is_empty()
        && !query.chars().any(char::is_whitespace)
        && !query.contains('/')
        && !query.contains('.')
        && !query.contains("::")
        && query
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && is_code_shaped_identifier(query)
}

fn is_code_shaped_identifier(value: &str) -> bool {
    let has_lower = value.chars().any(|ch| ch.is_ascii_lowercase());
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
    (has_lower && has_upper) || value.contains('_') || has_digit
}

fn is_qualified_symbol_query(query: &str) -> bool {
    if query.is_empty() || query.chars().any(char::is_whitespace) {
        return false;
    }
    query.contains("::")
        || (query.contains('.')
            && !is_source_path_token(query)
            && query.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            }))
}

fn contains_qualified_symbol_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_qualified_symbol_query)
}

fn contains_named_identifier_reference(query: &str) -> bool {
    query.split_whitespace().map(trim_query_token).any(|token| {
        if !is_exact_identifier_query(token) {
            return false;
        }
        let has_lower = token.chars().any(|ch| ch.is_ascii_lowercase());
        let has_upper = token.chars().any(|ch| ch.is_ascii_uppercase());
        let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        (has_lower && has_upper) || token.contains('_') || has_digit
    })
}

fn trim_query_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '"' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '.' | '!' | '?'
        )
    })
}

fn contains_path_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_source_path_token)
}

fn is_source_path_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("/api/") {
        return false;
    }
    lower.contains('/')
        || [
            ".rs", ".java", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".md", ".mdx", ".toml",
            ".yaml", ".yml", ".json",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn is_error_trace_query(lower: &str) -> bool {
    let explicit_error = contains_any(
        lower,
        &[
            "stack trace",
            "traceback",
            "exception:",
            "panic:",
            "panicked at",
            "caused by:",
        ],
    );
    let stack_frame = lower.lines().skip(1).any(|line| {
        let line = line.trim_start();
        line.starts_with("at ") || line.starts_with("file ")
    });
    explicit_error
        || (stack_frame && contains_any(lower, &["error", "exception", "panic", "traceback"]))
}

fn is_api_resource_query(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "/api/",
            "route ",
            "endpoint ",
            "config key",
            "configuration key",
            "resource ",
            "topic ",
            "queue ",
            "table ",
        ],
    )
}

fn natural_language_token_count(query: &str) -> usize {
    query
        .split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_ascii_alphabetic()))
        .count()
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
            route
                .policy
                .candidate_cap(RetrievalSourceKind::Validation, 10)
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
        assert!(
            route
                .policy
                .candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
                > lexical
        );
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
                > docs
                    .policy
                    .candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
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
        assert!(route
            .reasons
            .iter()
            .any(|reason| reason.contains("multiple")));
        for source in all_sources() {
            assert!(route.policy.allows(source));
        }
        assert!(route.confidence < 0.5);
    }

    #[test]
    fn contract_explanation_alone_does_not_claim_ripple_evidence_requirements() {
        let route = classify_task(
            "explain contract evidence for checkout summary in src/domain/checkout.rs",
        );
        assert_eq!(route.family, TaskFamily::General);
        assert!(!route.policy.missing_required_evidence_is_blocker);

        let explicit_ripple =
            classify_task("explain contract dependency boundary and callers for checkout");
        assert_eq!(explicit_ripple.family, TaskFamily::EditToRipple);
        assert!(explicit_ripple.policy.missing_required_evidence_is_blocker);
    }

    #[test]
    fn implementation_impact_and_boundary_terms_do_not_trigger_ripple_blockers() {
        for task in [
            "change plan engine boundary evidence",
            "improve impact analysis direct impacts",
        ] {
            let route = classify_task(task);
            assert_eq!(route.family, TaskFamily::General, "task: {task}");
            assert!(
                !route.policy.missing_required_evidence_is_blocker,
                "task: {task}"
            );
        }

        let explicit_ripple =
            classify_task("show the impact of changing checkout on downstream callers");
        assert_eq!(explicit_ripple.family, TaskFamily::EditToRipple);
        assert!(explicit_ripple.policy.missing_required_evidence_is_blocker);
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
        assert!(route
            .policy
            .required_evidence
            .contains(&RetrievalSourceKind::Runtime));
    }

    #[test]
    fn query_shape_distinguishes_exact_qualified_path_trace_and_conceptual_queries() {
        assert_eq!(
            classify_task("PlanEngine").query_shape,
            QueryShape::ExactIdentifier
        );
        assert_eq!(
            classify_task("open_kioku_context::ContextPackBuilder").query_shape,
            QueryShape::QualifiedSymbol
        );
        assert_eq!(
            classify_task("crates/open-kioku-context/src/routing.rs").query_shape,
            QueryShape::PathReference
        );
        assert_eq!(
            classify_task("panic: index corrupt\n at open_index").query_shape,
            QueryShape::ErrorTrace
        );
        assert_eq!(
            classify_task("how context selection avoids redundant evidence").query_shape,
            QueryShape::Conceptual
        );
    }

    #[test]
    fn mixed_structured_query_falls_back_to_broad_shape_without_weakening_required_evidence() {
        let route = classify_task(
            "find regression tests for ContextPackBuilder in crates/open-kioku-context/src/lib.rs",
        );
        assert_eq!(route.family, TaskFamily::CodeToTest);
        assert_eq!(
            route.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert_eq!(
            route.policy.required_evidence,
            vec![RetrievalSourceKind::Validation]
        );
        assert!(route.policy.missing_required_evidence_is_blocker);
        assert!(!route.query_shape_signals.is_empty());
    }

    #[test]
    fn exact_identifier_shape_favors_exact_and_lexical_without_changing_authority_or_sources() {
        let route = classify_task("PlanEngine");
        assert_eq!(route.family, TaskFamily::General);
        assert_eq!(route.query_shape, QueryShape::ExactIdentifier);
        assert!(
            route
                .policy
                .candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
                > route
                    .policy
                    .candidate_cap(RetrievalSourceKind::SemanticVector, 10)
        );
        assert!(route.policy.allows(RetrievalSourceKind::SemanticVector));
    }

    #[test]
    fn plain_single_word_query_stays_conservative_instead_of_claiming_exact_identity() {
        let route = classify_task("authentication");
        assert_eq!(route.query_shape, QueryShape::Unknown);
        assert!(route.query_shape_fallback_reason.is_some());
        for source in all_sources() {
            assert_eq!(route.policy.candidate_cap(source, 10), 40);
        }
    }

    #[test]
    fn punctuation_around_identifier_does_not_hide_structured_signal() {
        let route = classify_task("explain ContextPackBuilder. selection behavior");
        assert_eq!(
            route.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert!(route
            .query_shape_signals
            .iter()
            .any(|signal| signal.contains("identifier-shaped code reference")));
    }

    #[test]
    fn embedded_code_identifier_and_api_resource_shapes_are_not_lost_in_prose() {
        assert_eq!(
            classify_task("explain ContextPackBuilder selection behavior").query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert_eq!(
            classify_task("/api/v1/orders").query_shape,
            QueryShape::ApiResource
        );
        assert_eq!(
            classify_task("look at this behavior carefully").query_shape,
            QueryShape::Conceptual
        );
    }

    #[test]
    fn query_shape_refinement_preserves_specialized_family_top_tier() {
        let tests = classify_task("find tests for ContextPackBuilder");
        assert_eq!(tests.family, TaskFamily::CodeToTest);
        assert!(
            tests
                .policy
                .candidate_cap(RetrievalSourceKind::Validation, 10)
                > tests
                    .policy
                    .candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
        );

        let ripple = classify_task("show callers for ContextPackBuilder");
        assert_eq!(ripple.family, TaskFamily::EditToRipple);
        let lexical = ripple
            .policy
            .candidate_cap(RetrievalSourceKind::Lexical, 10);
        assert!(
            ripple
                .policy
                .candidate_cap(RetrievalSourceKind::ExactSemantic, 10)
                > lexical
        );
        assert!(ripple.policy.candidate_cap(RetrievalSourceKind::Graph, 10) > lexical);
    }

    #[test]
    fn diagnostics_preserve_query_shape_reasoning_for_json_and_mcp_consumers() {
        let diagnostics = classify_task("fix panic in src/index.rs").diagnostics();
        assert_eq!(
            diagnostics.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert!(diagnostics.query_shape_confidence > 0.0);
        assert!(!diagnostics.query_shape_signals.is_empty());
    }
}
