from pathlib import Path

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label}: expected exactly one match, found {count}')
    text = text.replace(old, new, 1)


old_case_validation = '''        if case.id.trim().is_empty()
            || case.query.trim().is_empty()
            || case.language.trim().is_empty()
            || case.base_revision.trim().is_empty()
        {
            anyhow::bail!(
                "retrieval case requires non-empty id, query, language, and base_revision"
            );
        }
        validate_safe_relative_path(&case.repo_fixture, "repo_fixture", &case.id)?;
        for gold in &case.gold_files {
            validate_safe_relative_path(gold, "gold_files", &case.id)?;
        }
'''
new_case_validation = '''        if case.id.trim().is_empty()
            || case.query.trim().is_empty()
            || case.language.trim().is_empty()
            || case.base_revision.trim().is_empty()
        {
            anyhow::bail!(
                "retrieval case requires non-empty id, query, language, and base_revision"
            );
        }
        validate_fixture_revision(&case.base_revision, &case.id)?;
        validate_safe_relative_path(&case.repo_fixture, "repo_fixture", &case.id)?;
        let mut gold_paths = std::collections::HashSet::new();
        for gold in &case.gold_files {
            validate_safe_relative_path(gold, "gold_files", &case.id)?;
            let normalized = normalize_path_fragment(&gold.to_string_lossy());
            if !gold_paths.insert(normalized) {
                anyhow::bail!("retrieval case `{}` contains a duplicate gold file", case.id);
            }
        }
'''
replace_once(old_case_validation, new_case_validation, 'case validation')

safe_path_marker = '''fn validate_safe_relative_path(path: &Path, field: &str, case_id: &str) -> anyhow::Result<()> {
'''
revision_helper = '''fn validate_fixture_revision(revision: &str, case_id: &str) -> anyhow::Result<()> {
    let Some(hex) = revision.strip_prefix("sha256:") else {
        anyhow::bail!(
            "retrieval case `{case_id}` base_revision must be a frozen sha256 digest"
        );
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!(
            "retrieval case `{case_id}` base_revision is not a valid sha256 digest"
        );
    }
    Ok(())
}

'''
replace_once(safe_path_marker, revision_helper + safe_path_marker, 'revision helper insertion')

old_fixture_block = '''    let fixtures = retrieval_fixture_paths(&root, &corpus.cases)?;
    let mut fixture_digests = BTreeMap::new();
'''
new_fixture_block = '''    let fixtures = retrieval_fixture_paths(&root, &corpus.cases)?;
    validate_retrieval_gold_files(&root, &corpus.cases)?;
    let mut fixture_digests = BTreeMap::new();
'''
replace_once(old_fixture_block, new_fixture_block, 'gold-file validation call')

revision_function_marker = '''fn retrieval_fixture_digest(fixture: &Path) -> anyhow::Result<String> {
'''
gold_helper = '''fn validate_retrieval_gold_files(root: &Path, cases: &[RetrievalCase]) -> anyhow::Result<()> {
    for case in cases {
        let fixture = root.join(&case.repo_fixture);
        for gold in &case.gold_files {
            let gold_path = fixture.join(gold);
            if !gold_path.is_file() {
                anyhow::bail!(
                    "retrieval case `{}` gold file does not exist: {}",
                    case.id,
                    gold_path.display()
                );
            }
        }
    }
    Ok(())
}

'''
replace_once(revision_function_marker, gold_helper + revision_function_marker, 'gold helper insertion')

old_revision_guard = '''    for case in cases {
        if !case.base_revision.starts_with("sha256:") {
            continue;
        }
        let key = root
'''
new_revision_guard = '''    for case in cases {
        let key = root
'''
replace_once(old_revision_guard, new_revision_guard, 'remove permissive revision guard')

unit_test_marker = '''    #[test]
    fn no_gold_cases_are_separate_from_positive_retrieval_metrics() {
'''
unit_tests = '''    #[test]
    fn frozen_revision_validation_rejects_unpinned_and_malformed_values() {
        assert!(validate_fixture_revision(
            "sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
            "valid"
        )
        .is_ok());
        assert!(validate_fixture_revision("main", "unpinned").is_err());
        assert!(validate_fixture_revision("sha256:not-a-digest", "malformed").is_err());
    }

    #[test]
    fn no_gold_cases_are_separate_from_positive_retrieval_metrics() {
'''
replace_once(unit_test_marker, unit_tests, 'revision unit test')

path.write_text(text)
