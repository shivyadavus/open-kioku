from pathlib import Path

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()

def replace_once(old, new, label):
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label}: expected one marker, found {count}')
    text = text.replace(old, new, 1)

replace_once(
'''    let query_shape_labels_path = cases_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("retrieval-query-shape-labels.json");
    let query_shape_labels = if query_shape_labels_path.is_file() {
        Some(load_and_apply_query_shape_labels(
            &query_shape_labels_path,
            &mut corpus,
        )?)
    } else {
        None
    };''',
'''    let query_shape_labels_path = query_shape_labels_path(&cases_file);
    let query_shape_labels = match query_shape_labels_path.as_ref() {
        Some(path) if path.is_file() => Some(load_and_apply_query_shape_labels(path, &mut corpus)?),
        _ => None,
    };''',
'corpus-derived sidecar discovery',
)

replace_once(
'''    let query_shape_benchmark = query_shape_labels
        .as_ref()
        .map(|labels| build_query_shape_benchmark(&corpus, labels, &query_shape_labels_path))
        .transpose()?;''',
'''    let query_shape_benchmark = match (&query_shape_labels, &query_shape_labels_path) {
        (Some(labels), Some(path)) => Some(build_query_shape_benchmark(&corpus, labels, path)?),
        _ => None,
    };''',
'optional sidecar benchmark build',
)

marker = '''fn load_and_apply_query_shape_labels(
    path: &Path,
    corpus: &mut RetrievalCorpus,
) -> anyhow::Result<RetrievalQueryShapeLabels> {'''
helper = '''fn query_shape_labels_path(cases_file: &Path) -> Option<PathBuf> {
    let stem = cases_file.file_stem()?.to_str()?;
    let prefix = stem.strip_suffix("-cases")?;
    Some(
        cases_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{prefix}-query-shape-labels.json")),
    )
}

'''+marker
replace_once(marker, helper, 'sidecar path helper')

replace_once(
'''    Ok(RetrievalQueryShapeBenchmark {
        labels_file: labels_path.to_path_buf(),''',
'''    let report_labels_file = labels_path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("query-shape-labels.json"));
    Ok(RetrievalQueryShapeBenchmark {
        labels_file: report_labels_file,''',
'privacy-safe report label path',
)

# Add focused path-discovery/privacy regression near query-shape tests.
marker = '''    #[test]
    fn query_shape_labels_fail_closed_for_missing_or_unknown_case_ids() {'''
test = '''    #[test]
    fn query_shape_sidecar_discovery_is_corpus_derived_and_report_path_is_portable() {
        assert_eq!(
            query_shape_labels_path(Path::new("benchmarks/retrieval-cases.json")),
            Some(PathBuf::from("benchmarks/retrieval-query-shape-labels.json"))
        );
        assert_eq!(
            query_shape_labels_path(Path::new("benchmarks/custom-cases.json")),
            Some(PathBuf::from("benchmarks/custom-query-shape-labels.json"))
        );
        assert_eq!(query_shape_labels_path(Path::new("benchmarks/custom.json")), None);
    }

'''+marker
replace_once(marker, test, 'sidecar discovery test')

path.write_text(text)
