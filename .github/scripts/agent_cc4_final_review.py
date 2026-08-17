from pathlib import Path
import re

path = Path("crates/open-kioku-context/src/routing.rs")
text = path.read_text()

# Recognize structured code references embedded in natural-language tasks, not just whole-query
# identifiers. The shape remains Mixed when prose surrounds the reference.
pattern = re.compile(
    r"    if is_qualified_symbol_query\(trimmed\) \{\n.*?    \} else if is_exact_identifier_query\(trimmed\) \{\n.*?    \}\n",
    re.S,
)
replacement = '''    if is_qualified_symbol_query(trimmed) {
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
'''
text, count = pattern.subn(replacement, text, count=1)
if count != 1:
    raise SystemExit(f"embedded identifier classifier replacement count={count}")

# Query shape may refine breadth, but it must not erase the ordering encoded by a specialized
# parent task-family policy. Flat policies (General/Issue) may be differentiated by shape.
pattern = re.compile(
    r"            let refined = factor\.saturating_add\(\*delta\);\n"
    r"            \*factor = if flat_family \{\n"
    r"                refined\n"
    r"            \} else \{\n"
    r"                refined\.min\(family_max\)\n"
    r"            \};"
)
replacement = '''            let original = *factor;
            let refined = original.saturating_add(*delta);
            *factor = if flat_family {
                refined
            } else if original == family_max {
                original
            } else {
                refined.min(family_max.saturating_sub(1).max(original))
            };'''
text, count = pattern.subn(replacement, text, count=1)
if count != 1:
    raise SystemExit(f"family priority refinement replacement count={count}")

marker = "fn contains_path_reference(query: &str) -> bool {\n"
helpers = '''fn contains_qualified_symbol_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_qualified_symbol_query)
}

fn contains_named_identifier_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(|token| {
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
            '`' | '\'' | '"' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
        )
    })
}

fn contains_path_reference(query: &str) -> bool {
'''
if text.count(marker) != 1:
    raise SystemExit(f"query-token helper insertion marker count={text.count(marker)}")
text = text.replace(marker, helpers, 1)

pattern = re.compile(
    r"fn contains_path_reference\(query: &str\) -> bool \{\n"
    r"    query\n"
    r"        \.split_whitespace\(\)\n"
    r"        \.map\(\|token\| \{.*?        \}\)\n"
    r"        \.any\(is_source_path_token\)\n"
    r"\}",
    re.S,
)
replacement = '''fn contains_path_reference(query: &str) -> bool {
    query
        .split_whitespace()
        .map(trim_query_token)
        .any(is_source_path_token)
}'''
text, count = pattern.subn(replacement, text, count=1)
if count != 1:
    raise SystemExit(f"path token normalization replacement count={count}")

# API resource names are structured resources, not repository paths merely because they contain '/'.
pattern = re.compile(
    r"fn is_source_path_token\(token: &str\) -> bool \{\n"
    r"    let lower = token\.to_ascii_lowercase\(\);\n"
    r"    lower\.contains\('/'\)"
)
replacement = '''fn is_source_path_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("/api/") {
        return false;
    }
    lower.contains('/')'''
text, count = pattern.subn(replacement, text, count=1)
if count != 1:
    raise SystemExit(f"API-vs-path replacement count={count}")

marker = '''    #[test]
    fn diagnostics_preserve_query_shape_reasoning_for_json_and_mcp_consumers() {
'''
tests = '''    #[test]
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
        let lexical = ripple.policy.candidate_cap(RetrievalSourceKind::Lexical, 10);
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
'''
if text.count(marker) != 1:
    raise SystemExit(f"self-review test insertion marker count={text.count(marker)}")
text = text.replace(marker, tests, 1)

path.write_text(text)
