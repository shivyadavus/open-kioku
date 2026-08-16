from pathlib import Path


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label}: expected exactly one match, found {count}')
    path.write_text(text.replace(old, new, 1))


retrieval = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
replace_once(
    retrieval,
    '''#[derive(Debug, Clone, Deserialize)]
struct RetrievalCorpus {''',
    '''#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalCorpus {''',
    'strict corpus schema',
)
replace_once(
    retrieval,
    '''#[derive(Debug, Clone, Deserialize)]
struct RetrievalCase {''',
    '''#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalCase {''',
    'strict case schema',
)

test_marker = '''    #[test]
    fn frozen_revision_validation_rejects_unpinned_and_malformed_values() {
'''
test_block = '''    #[test]
    fn corpus_schema_rejects_unknown_fields() {
        let unknown_corpus = r#"{
            "schema_version": "1.0.0",
            "corpus_id": "strict",
            "token_budgets": [2000],
            "cases": [],
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<RetrievalCorpus>(unknown_corpus).is_err());

        let unknown_case = r#"{
            "schema_version": "1.0.0",
            "corpus_id": "strict",
            "token_budgets": [2000],
            "cases": [{
                "id": "case",
                "task_family": "issue_to_code",
                "language": "rust",
                "repo_fixture": "fixture",
                "base_revision": "sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
                "split": "holdout",
                "query": "query",
                "gold_files": ["src/lib.rs"],
                "unexpected": true
            }]
        }"#;
        assert!(serde_json::from_str::<RetrievalCorpus>(unknown_case).is_err());
    }

    #[test]
    fn frozen_revision_validation_rejects_unpinned_and_malformed_values() {
'''
replace_once(retrieval, test_marker, test_block, 'strict schema test')

readme = Path('README.md')
workflow_section = '''Use `ok workflow-bench` for plan → edit → verify benchmark cases:

```sh
ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
```

See [`docs/workflow-benchmarks.md`](docs/workflow-benchmarks.md) for the case format and rollup metrics.
'''
retrieval_section = workflow_section + '''
Use `ok retrieval-bench` to measure repository-context retrieval independently of patch generation:

```sh
ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30
```

The bundled frozen corpus and regression policy are documented in [`docs/retrieval-benchmark.md`](docs/retrieval-benchmark.md).
'''
replace_once(readme, workflow_section, retrieval_section, 'README retrieval benchmark section')

old_inventory = 'Current top-level commands (36): `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `contract`, `bench`, `workflow-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`.'
new_inventory = 'Current top-level commands (37): `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `contract`, `bench`, `workflow-bench`, `retrieval-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`.'
replace_once(readme, old_inventory, new_inventory, 'README command inventory')

replace_once(
    readme,
    '- `open-kioku-cli`: the `ok` binary (33 subcommands).',
    '- `open-kioku-cli`: the `ok` binary and top-level command surface.',
    'README CLI crate description',
)

replace_once(
    readme,
    '''ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
ok --repo . history bench --cases-file benchmarks/history-cases.json
''',
    '''ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30
ok --repo . history bench --cases-file benchmarks/history-cases.json
''',
    'README development commands',
)
