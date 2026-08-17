from pathlib import Path

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()
text = text.replace(
    '    #[test]\n    #[test]\n    fn retrieval_report_groups_quality_by_frozen_query_shape()',
    '    #[test]\n    fn retrieval_report_groups_quality_by_frozen_query_shape()',
    1,
)
old = '''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            language: "rust".into(),
'''
new = '''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            query_shape: open_kioku_core::QueryShape::Conceptual,
            classified_query_shape: open_kioku_core::QueryShape::Conceptual,
            query_shape_match: true,
            language: "rust".into(),
'''
if text.count(old) != 1:
    raise SystemExit(f'report helper marker count={text.count(old)}')
text = text.replace(old, new, 1)
text = text.replace('assert_eq!(comparisons.len(), 2);', 'assert_eq!(comparisons.len(), 4);', 1)
old = '''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
'''
new = '''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
        assert_eq!(comparisons[2].scope, "query_shape:conceptual");
        assert_eq!(
            comparisons[3].scope,
            "task_family_query_shape:issue_to_code/conceptual"
        );
'''
if text.count(old) != 1:
    raise SystemExit(f'comparison test marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''    fn corpus_schema_rejects_unknown_fields() {
'''
new = '''    #[test]
    fn corpus_schema_rejects_unknown_fields() {
'''
if text.count(old) != 1:
    raise SystemExit(f'corpus schema test marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
