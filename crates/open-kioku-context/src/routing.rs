use open_kioku_core::{
    RetrievalRoutingDiagnostics, RetrievalSourceKind, TaskFamily,
};

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
    pub source_priors: Vec<(RetrievalSourceKind, f32)>,
}

impl RetrievalPolicy {
    pub fn allows(&self, source: RetrievalSourceKind) -> bool {
        self.enabled_sources.contains(&source)
    }

    pub fn apply_source_priors(
        &self,
        weights: &mut std::collections::BTreeMap<RetrievalSourceKind, f32>,
    ) {
        for source in all_sources() {
            if !self.allows(source) {
                weights.insert(source, 0.0);
            }
        }
        for (source, prior) in &self.source_priors {
            if self.allows(*source) {
                if let Some(weight) = weights.get_mut(source) {
                    *weight *= *prior;
                }
            }
        }
    }
}

pub fn classify_task(task: &str) -> TaskRoutingDecision {
    let lower = task.to_ascii_lowercase();
    let has_doc = contains_any(
        &lower,
        &["document", "documentation", "docs", "readme", "guide", "adr", "markdown"],
    );
    let has_code = contains_any(
        &lower,
        &["implement", "code", "function", "method", "class", "module", "api", "behavior", "change"],
    );
    let has_test = contains_any(
        &lower,
        &["test", "tests", "fixture", "coverage", "validation", "verify", "regression"],
    );
    let has_trace = contains_any(
        &lower,
        &["stack trace", "traceback", "exception", "runtime error", "crash", "panic", "incident", "failure log"],
    );
    let has_review = contains_any(
        &lower,
        &["review comment", "review feedback", "requested changes", "pull request comment", "pr comment"],
    );
    let has_ripple = contains_any(
        &lower,
        &["impact", "ripple", "callers", "callees", "dependent", "dependency", "public api", "contract", "boundary"],
    );
    let has_issue = contains_any(
        &lower,
        &["issue", "ticket", "bug", "feature", "request", "implement", "fix"],
    );

    if has_doc && has_code {
        return decision(
            TaskFamily::MixedCodeDocs,
            0.90,
            vec!["task contains both documentation and implementation/change language".into()],
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
            vec!["task explicitly targets tests, validation, coverage, or regression evidence".into()],
        );
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
        vec!["no deterministic task-family rule matched; using conservative general retrieval".into()],
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
            source_priors: vec![(S::Document, 1.50), (S::ExactSemantic, 1.00)],
        },
        TaskFamily::MixedCodeDocs => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Document, S::Lexical],
            source_priors: vec![(S::Document, 1.20), (S::ExactSemantic, 1.15), (S::Validation, 1.10)],
        },
        TaskFamily::CodeToTest => RetrievalPolicy {
            enabled_sources: vec![S::Lexical, S::ExactSemantic, S::Graph, S::Validation, S::GitHistory, S::Runtime],
            required_evidence: vec![S::Validation],
            source_priors: vec![(S::Validation, 1.50), (S::ExactSemantic, 1.20), (S::GitHistory, 1.10)],
        },
        TaskFamily::TraceToCode => RetrievalPolicy {
            enabled_sources: vec![S::Lexical, S::ExactSemantic, S::Graph, S::Validation, S::GitHistory, S::Runtime],
            required_evidence: vec![S::Runtime],
            source_priors: vec![(S::Runtime, 1.55), (S::Graph, 1.25), (S::ExactSemantic, 1.20)],
        },
        TaskFamily::CommentToContext => RetrievalPolicy {
            enabled_sources: vec![S::Lexical, S::Document, S::ExactSemantic, S::Graph, S::Validation, S::GitHistory],
            required_evidence: vec![S::Lexical],
            source_priors: vec![(S::ExactSemantic, 1.25), (S::Lexical, 1.15), (S::Graph, 1.10)],
        },
        TaskFamily::EditToRipple => RetrievalPolicy {
            enabled_sources: vec![S::Lexical, S::ExactSemantic, S::Graph, S::Validation, S::GitHistory, S::Runtime],
            required_evidence: vec![S::ExactSemantic, S::Graph],
            source_priors: vec![(S::ExactSemantic, 1.45), (S::Graph, 1.40), (S::Validation, 1.10)],
        },
        TaskFamily::IssueToCode => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: vec![S::Lexical],
            source_priors: vec![(S::ExactSemantic, 1.20), (S::Graph, 1.10), (S::Validation, 1.10)],
        },
        TaskFamily::General => RetrievalPolicy {
            enabled_sources: all_sources().to_vec(),
            required_evidence: Vec::new(),
            source_priors: Vec::new(),
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
    fn code_to_test_requires_and_prioritizes_validation_without_document_noise() {
        let route = classify_task("find regression tests and validation for changed parser code");
        assert_eq!(route.family, TaskFamily::CodeToTest);
        assert!(route.policy.allows(RetrievalSourceKind::Validation));
        assert!(!route.policy.allows(RetrievalSourceKind::Document));
        assert_eq!(route.policy.required_evidence, vec![RetrievalSourceKind::Validation]);

        let mut weights = std::collections::BTreeMap::from([
            (RetrievalSourceKind::Validation, 1.0),
            (RetrievalSourceKind::Lexical, 1.0),
            (RetrievalSourceKind::Document, 1.0),
        ]);
        route.policy.apply_source_priors(&mut weights);
        assert!(weights[&RetrievalSourceKind::Validation] > weights[&RetrievalSourceKind::Lexical]);
        assert_eq!(weights[&RetrievalSourceKind::Document], 0.0);
    }

    #[test]
    fn edit_to_ripple_requires_exact_and_graph_evidence() {
        let route = classify_task("show ripple impact across callers, dependencies, and public API boundary");
        assert_eq!(route.family, TaskFamily::EditToRipple);
        assert_eq!(
            route.policy.required_evidence,
            vec![RetrievalSourceKind::ExactSemantic, RetrievalSourceKind::Graph]
        );
    }

    #[test]
    fn documentation_only_reserves_document_stream_while_mixed_keeps_both_domains() {
        let docs = classify_task("document contributor workflow in the guide");
        assert_eq!(docs.family, TaskFamily::Documentation);
        assert!(docs.policy.allows(RetrievalSourceKind::Document));
        assert!(!docs.policy.allows(RetrievalSourceKind::Lexical));

        let mixed = classify_task("change retry behavior and update documentation for the API");
        assert_eq!(mixed.family, TaskFamily::MixedCodeDocs);
        assert!(mixed.policy.allows(RetrievalSourceKind::Document));
        assert!(mixed.policy.allows(RetrievalSourceKind::Lexical));
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
    fn classification_does_not_change_evidence_authority() {
        let route = classify_task("trace runtime error to implementation");
        assert_eq!(route.family, TaskFamily::TraceToCode);
        assert!(route.policy.source_priors.iter().any(|(source, prior)| {
            *source == RetrievalSourceKind::Runtime && *prior > 1.0
        }));
        // Policies only change participation/priors. Authority is owned by candidate evidence.
        assert!(route.policy.required_evidence.contains(&RetrievalSourceKind::Runtime));
    }
}
