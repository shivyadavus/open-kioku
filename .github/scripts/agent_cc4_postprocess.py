from pathlib import Path
import json

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()
text = text.replace(
    'const RETRIEVAL_BENCH_SCHEMA_VERSION: &str = "1.0.0";',
    'const RETRIEVAL_BENCH_SCHEMA_VERSION: &str = "1.1.0";',
    1,
)
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
# Make the unknown-field case otherwise valid under schema 1.1 so the regression proves
# deny_unknown_fields rather than accidentally failing because query_shape is missing.
old = '''                "task_family": "issue_to_code",
                "language": "rust",
                "repo_fixture": "fixture",
'''
new = '''                "task_family": "issue_to_code",
                "query_shape": "conceptual",
                "language": "rust",
                "repo_fixture": "fixture",
'''
if text.count(old) != 1:
    raise SystemExit(f'unknown-case query-shape marker count={text.count(old)}')
text = text.replace(old, new, 1)
text = text.replace('"schema_version": "1.0.0",\n            "corpus_id": "strict"', '"schema_version": "1.1.0",\n            "corpus_id": "strict"')
path.write_text(text)

# query_shape is a new required corpus field, so this is a backwards-incompatible schema
# shape change. Bump only schema metadata; do not alter frozen metric values or thresholds.
for filename in (
    'benchmarks/retrieval-cases.json',
    'benchmarks/retrieval-baseline.json',
    'benchmarks/retrieval-thresholds.json',
):
    p = Path(filename)
    data = json.loads(p.read_text())
    if data.get('schema_version') != '1.0.0':
        raise SystemExit(f'{filename}: expected schema_version 1.0.0 before migration')
    data['schema_version'] = '1.1.0'
    p.write_text(json.dumps(data, indent=2) + '\n')
