from pathlib import Path

path = Path("crates/open-kioku-context/src/routing.rs")
text = path.read_text()

old = '''fn is_exact_identifier_query(query: &str) -> bool {
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
'''
new = '''fn is_exact_identifier_query(query: &str) -> bool {
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
'''
if text.count(old) != 1:
    raise SystemExit(f"exact identifier marker count={text.count(old)}")
text = text.replace(old, new, 1)

old = '''fn trim_query_token(token: &str) -> &str {
    token.trim_matches(|ch: char| matches!(ch, '`' | '"' | ',' | ';' | ':' | '(' | ')' | '[' | ']'))
}
'''
new = '''fn trim_query_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '"' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '.' | '!' | '?'
        )
    })
}
'''
if text.count(old) != 1:
    raise SystemExit(f"token trim marker count={text.count(old)}")
text = text.replace(old, new, 1)

marker = '''    #[test]
    fn embedded_code_identifier_and_api_resource_shapes_are_not_lost_in_prose() {
'''
tests = '''    #[test]
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
        assert_eq!(route.query_shape, QueryShape::MixedStructuredNaturalLanguage);
        assert!(route
            .query_shape_signals
            .iter()
            .any(|signal| signal.contains("identifier-shaped code reference")));
    }

'''
if text.count(marker) != 1:
    raise SystemExit(f"test insertion marker count={text.count(marker)}")
text = text.replace(marker, tests + marker, 1)
path.write_text(text)
