from pathlib import Path
import re

# Extend shared diagnostics with deterministic query-shape metadata.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
marker = '''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalRoutingDiagnostics {
    pub task_family: TaskFamily,
    pub confidence: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub enabled_sources: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub required_evidence: Vec<RetrievalSourceKind>,
}
'''
replacement = '''#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryShape {
    ExactIdentifier,
    QualifiedSymbol,
    PathReference,
    ErrorTrace,
    ApiResource,
    Conceptual,
    MixedStructuredNaturalLanguage,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalRoutingDiagnostics {
    pub task_family: TaskFamily,
    pub confidence: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub enabled_sources: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub required_evidence: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub query_shape: QueryShape,
    #[serde(default)]
    pub query_shape_confidence: f32,
    #[serde(default)]
    pub query_shape_signals: Vec<String>,
    #[serde(default)]
    pub query_shape_ambiguities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_shape_fallback_reason: Option<String>,
}
'''
if text.count(marker) != 1:
    raise SystemExit(f'core routing marker count={text.count(marker)}')
text = text.replace(marker, replacement, 1)
old_default = '''            reasons: Vec::new(),
            enabled_sources: Vec::new(),
            required_evidence: Vec::new(),
        }
'''
new_default = '''            reasons: Vec::new(),
            enabled_sources: Vec::new(),
            required_evidence: Vec::new(),
            query_shape: QueryShape::Unknown,
            query_shape_confidence: 0.0,
            query_shape_signals: Vec::new(),
            query_shape_ambiguities: Vec::new(),
            query_shape_fallback_reason: None,
        }
'''
# Restrict replacement to the RetrievalRoutingDiagnostics impl body.
default_start = text.index('impl Default for RetrievalRoutingDiagnostics')
default_end = text.index('\n}\n\n#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]', default_start) + 2
default_block = text[default_start:default_end]
if default_block.count(old_default) != 1:
    raise SystemExit(f'core routing default marker count={default_block.count(old_default)}')
default_block = default_block.replace(old_default, new_default, 1)
text = text[:default_start] + default_block + text[default_end:]
path.write_text(text)

# Integrate query-shape classification into the existing task routing path.
path = Path('crates/open-kioku-context/src/routing.rs')
text = path.read_text()
old_import = 'use open_kioku_core::{RetrievalRoutingDiagnostics, RetrievalSourceKind, TaskFamily};\n'
new_import = 'use open_kioku_core::{QueryShape, RetrievalRoutingDiagnostics, RetrievalSourceKind, TaskFamily};\n'
if text.count(old_import) != 1:
    raise SystemExit(f'routing import marker count={text.count(old_import)}')
text = text.replace(old_import, new_import, 1)

old_struct = '''pub struct TaskRoutingDecision {
    pub family: TaskFamily,
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub policy: RetrievalPolicy,
}
'''
new_struct = '''pub struct TaskRoutingDecision {
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
'''
if text.count(old_struct) != 1:
    raise SystemExit(f'routing decision struct marker count={text.count(old_struct)}')
text = text.replace(old_struct, new_struct, 1)

old_diag = '''            reasons: self.reasons.clone(),
            enabled_sources: self.policy.enabled_sources.clone(),
            required_evidence: self.policy.required_evidence.clone(),
        }
'''
new_diag = '''            reasons: self.reasons.clone(),
            enabled_sources: self.policy.enabled_sources.clone(),
            required_evidence: self.policy.required_evidence.clone(),
            query_shape: self.query_shape,
            query_shape_confidence: self.query_shape_confidence,
            query_shape_signals: self.query_shape_signals.clone(),
            query_shape_ambiguities: self.query_shape_ambiguities.clone(),
            query_shape_fallback_reason: self.query_shape_fallback_reason.clone(),
        }
'''
if text.count(old_diag) != 1:
    raise SystemExit(f'routing diagnostics marker count={text.count(old_diag)}')
text = text.replace(old_diag, new_diag, 1)

# All classify_task calls to decision now pass the original task so one shared query-shape decision
# is applied consistently without introducing a second routing subsystem.
fn_marker = '\nfn decision(family: TaskFamily, confidence: f32, reasons: Vec<String>) -> TaskRoutingDecision {'
if text.count(fn_marker) != 1:
    raise SystemExit(f'decision function marker count={text.count(fn_marker)}')
prefix, suffix = text.split(fn_marker, 1)
prefix = prefix.replace('decision(', 'decision(task, ')
new_fn = '''
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
'''
# Remove old decision function body from suffix up to policy_for.
old_body_end = suffix.index('\nfn policy_for(')
suffix = suffix[old_body_end:]
text = prefix + new_fn + suffix

old_policy_head = '''fn policy_for(family: TaskFamily) -> RetrievalPolicy {
    use RetrievalSourceKind as S;
    match family {
'''
new_policy_head = '''fn policy_for(family: TaskFamily, query_shape: QueryShape) -> RetrievalPolicy {
    use RetrievalSourceKind as S;
    let mut policy = match family {
'''
if text.count(old_policy_head) != 1:
    raise SystemExit(f'policy head marker count={text.count(old_policy_head)}')
text = text.replace(old_policy_head, new_policy_head, 1)

policy_tail = '''    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
'''
helpers = '''    };
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

    if structured.len() > 1 || (structured.len() == 1 && natural_language && !single_structured_query(trimmed)) {
        let signals = structured.iter().map(|(_, signal)| signal.clone()).collect::<Vec<_>>();
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
    let boosts: &[(S, usize)] = match shape {
        QueryShape::ExactIdentifier | QueryShape::QualifiedSymbol => &[
            (S::ExactSemantic, 6),
            (S::Lexical, 5),
            (S::Graph, 3),
        ],
        QueryShape::PathReference => &[
            (S::Lexical, 6),
            (S::ExactSemantic, 4),
            (S::SemanticVector, 3),
        ],
        QueryShape::ErrorTrace => &[
            (S::Runtime, 6),
            (S::ExactSemantic, 5),
            (S::Lexical, 4),
            (S::Graph, 4),
        ],
        QueryShape::ApiResource => &[
            (S::ExactSemantic, 5),
            (S::Lexical, 5),
            (S::Graph, 4),
            (S::Runtime, 3),
        ],
        QueryShape::Conceptual => &[
            (S::SemanticVector, 6),
            (S::Lexical, 5),
            (S::Graph, 4),
            (S::Document, 4),
        ],
        QueryShape::MixedStructuredNaturalLanguage => &[
            (S::ExactSemantic, 5),
            (S::Lexical, 5),
            (S::SemanticVector, 4),
            (S::Graph, 4),
            (S::Document, 3),
            (S::Runtime, 3),
        ],
        QueryShape::Unknown => &[],
    };

    for (source, minimum_factor) in boosts {
        if !policy.allows(*source) {
            continue;
        }
        if let Some((_, factor)) = policy
            .candidate_factors
            .iter_mut()
            .find(|(candidate_source, _)| candidate_source == source)
        {
            *factor = (*factor).max(*minimum_factor);
        } else {
            policy.candidate_factors.push((*source, *minimum_factor));
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
        && query.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn is_qualified_symbol_query(query: &str) -> bool {
    if query.is_empty() || query.chars().any(char::is_whitespace) {
        return false;
    }
    query.contains("::")
        || (query.contains('.')
            && !is_source_path_token(query)
            && query
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')))
}

fn contains_path_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ',' | ';' | ':' | '(' | ')' | '[' | ']')))
        .any(is_source_path_token)
}

fn is_source_path_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.contains('/')
        || [".rs", ".java", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".md", ".mdx", ".toml", ".yaml", ".yml", ".json"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

fn is_error_trace_query(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "stack trace",
            "traceback",
            "exception:",
            "panic:",
            "panicked at",
            "caused by:",
            " at ",
        ],
    ) || (lower.contains('\n') && contains_any(lower, &["error", "exception", "panic", "traceback"]))
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
'''
if text.count(policy_tail) != 1:
    raise SystemExit(f'policy tail marker count={text.count(policy_tail)}')
text = text.replace(policy_tail, helpers, 1)

# Add adversarial/unit coverage at the routing boundary. These validate policy composition rather
# than benchmark outcomes so they cannot become benchmark-specific ranking hacks.
tests = '''

    #[test]
    fn query_shape_distinguishes_exact_qualified_path_trace_and_conceptual_queries() {
        assert_eq!(classify_task("PlanEngine").query_shape, QueryShape::ExactIdentifier);
        assert_eq!(
            classify_task("open_kioku_context::ContextPackBuilder").query_shape,
            QueryShape::QualifiedSymbol
        );
        assert_eq!(
            classify_task("crates/open-kioku-context/src/routing.rs").query_shape,
            QueryShape::PathReference
        );
        assert_eq!(
            classify_task("panic: index corrupt\\n at open_index").query_shape,
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
        assert_eq!(route.query_shape, QueryShape::MixedStructuredNaturalLanguage);
        assert_eq!(
            route.policy.required_evidence,
            vec![RetrievalSourceKind::Validation]
        );
        assert!(route.policy.missing_required_evidence_is_blocker);
        assert!(!route.query_shape_ambiguities.is_empty());
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
    fn diagnostics_preserve_query_shape_reasoning_for_json_and_mcp_consumers() {
        let diagnostics = classify_task("fix panic in src/index.rs").diagnostics();
        assert_eq!(
            diagnostics.query_shape,
            QueryShape::MixedStructuredNaturalLanguage
        );
        assert!(diagnostics.query_shape_confidence > 0.0);
        assert!(!diagnostics.query_shape_signals.is_empty());
    }
'''
if not text.endswith('}\n'):
    raise SystemExit('routing.rs no longer ends with test module brace')
text = text[:-2] + tests + '}\n'
path.write_text(text)
