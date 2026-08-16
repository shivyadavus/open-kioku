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
    pub candidate_multiplier: usize,
}

impl RetrievalPolicy {
    pub fn allows(&self, source: RetrievalSourceKind) -> bool {
        self.enabled_sources.contains(&source)
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
        &["issue", "ticket", "bug", "feature", "request", "implement", "fix"],
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
    if has_trace {
        let mut reasons = vec!["task contains runtime failure or trace language".into()];
        if has_test || has_ripple || has_issue {
            reasons.push(
                "multiple task-family signals matched; runtime failure evidence takes safety precedence"
                    .into(),
            );
        }
        return decision(TaskFamily::TraceToCode, 0.90, reasons);
    }
    if has_review {
        let mut reasons = vec!["task explicitly references review feedback/comment context".into()];
        if has_test || has_ripple || has_issue {
            reasons.push(
                "multiple task-family signals matched; review context remains the primary routing anchor"
                    .into(),
            );
        }
        return decision(TaskFamily::CommentToContext, 0.90, reasons);
    }
    if has_ripple {
        let mut reasons =
            vec!["task asks for dependency, impact, contract, or boundary context".into()];
        if has_test || has_issue {
            reasons.push(
                "multiple task-family signals matched; dependency/impact evidence takes structural precedence"
                    .into(),
            );
        }
        return decision(TaskFamily::EditToRipple, 0.86, reasons);
    }
    if has_test {
        let mut reasons = vec![
            "task explicitly targets tests, validation, coverage, or regression evidence".into(),
        ];
        if has_issue {
            reasons.push(
                "multiple task-family signals matched; validation intent is more specific than general issue-to-code"
                    .into(),
            );
        }
        return decision(TaskFamily::CodeToTest, 0.84, reasons);
    }
    if has_issue {
        return decision(
            TaskFamily::IssueToCode,
            0.78,
            vec!["task uses issue/change implementation language without a more specific family".into()],
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
            candidate_multiplier: 6,
        },
        TaskFamily::MixedCodeDocs => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Document, S::Lexical],
            candidate_multiplier: 5,
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
            candidate_multiplier: 5,
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
            candidate_multiplier: 5,
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
            candidate_multiplier: 4,
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
            candidate_multiplier: 5,
        },
        TaskFamily::IssueToCode => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Lexical],
            candidate_multiplier: 4,
        },
        TaskFamily::General => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: Vec::new(),
            candidate_multiplier: 4,
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
    fn code_to_test_requires_validation_without_document_noise() {
        let route = classify_task("find regression tests and validation for changed parser code");
        assert_eq!(route.family, TaskFamily::CodeToTest);
        assert!(route.policy.allows(RetrievalSourceKind::Validation));
        assert!(!route.policy.allows(RetrievalSourceKind::Document));
        assert_eq!(
            route.policy.required_evidence,
            vec![RetrievalSourceKind::Validation]
        );
        assert!(route.policy.candidate_multiplier >= 4);
    }

    #[test]
    fn edit_to_ripple_requires_exact_and_graph_evidence() {
        let route = classify_task(
            "show ripple impact across callers, dependencies, and public API boundary",
        );
        assert_eq!(route.family, TaskFamily::EditToRipple);
        assert_eq!(
            route.policy.required_evidence,
            vec![
                RetrievalSourceKind::ExactSemantic,
                RetrievalSourceKind::Graph
            ]
        );
    }

    #[test]
    fn documentation_only_reserves_document_stream_while_mixed_keeps_both_domains() {
        let docs = classify_task("change documentation in the contributor guide");
        assert_eq!(docs.family, TaskFamily::Documentation);
        assert!(docs.policy.allows(RetrievalSourceKind::Document));
        assert!(!docs.policy.allows(RetrievalSourceKind::Lexical));

        let api_docs = classify_task("update the API guide with authentication examples");
        assert_eq!(api_docs.family, TaskFamily::Documentation);

        let mixed = classify_task("change retry behavior in the method and update documentation");
        assert_eq!(mixed.family, TaskFamily::MixedCodeDocs);
        assert!(mixed.policy.allows(RetrievalSourceKind::Document));
        assert!(mixed.policy.allows(RetrievalSourceKind::Lexical));
    }

    #[test]
    fn ambiguous_runtime_failure_exposes_routing_precedence() {
        let route = classify_task("fix panic and add regression tests for dependency impact");
        assert_eq!(route.family, TaskFamily::TraceToCode);
        assert!(route.reasons.iter().any(|reason| reason.contains("multiple")));
        assert!(route.policy.required_evidence.contains(&RetrievalSourceKind::Runtime));
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
        assert!(route.policy.allows(RetrievalSourceKind::Runtime));
        assert!(route.policy.required_evidence.contains(&RetrievalSourceKind::Runtime));
    }
}
