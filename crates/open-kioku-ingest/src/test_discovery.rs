//! Where a test runner's configuration overrides the default discovery rules extraction
//! applies.
//!
//! Extraction judges a test-file callable by its runner's default rules, file by file. pytest
//! can be told to collect other files, classes and functions (`python_files`,
//! `python_classes`, `python_functions`), and a repository that does so would otherwise read as
//! one whose tests are all helpers. Under such a configuration, Python test-file callables are
//! judged by the test-path rule extraction used before runner rules: every one is a test. That
//! over-counts helpers there, which a quality note discloses, rather than hide the tests.

use open_kioku_core::{
    Confidence, File, FileId, Language, QualityNote, QualityNoteKind, ScoreComponent, TestTarget,
    TestTargetOrigin,
};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// The pytest discovery options that change which callables it collects.
const PYTEST_DISCOVERY_OPTIONS: [&str; 3] = ["python_files", "python_classes", "python_functions"];

/// pytest's configuration files, in the order it prefers them in one directory, with the
/// section that holds its options in each.
const PYTEST_CONFIG_FILES: [(&str, &str); 4] = [
    ("pytest.ini", "[pytest]"),
    ("pyproject.toml", "[tool.pytest.ini_options]"),
    ("tox.ini", "[pytest]"),
    ("setup.cfg", "[tool:pytest]"),
];

const CONFIGURED_DISCOVERY_REASON: &str =
    "callable in a test-path file under a pytest configuration that changes test discovery";

/// The pytest configuration governing one directory, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PytestConfig {
    path: PathBuf,
    /// The discovery options it sets, in [`PYTEST_DISCOVERY_OPTIONS`] order.
    discovery_options: Vec<&'static str>,
}

/// Re-reads every Python helper target under a pytest configuration that sets a discovery
/// option as a test, and returns one note per such configuration.
pub(crate) fn widen_configured_python_tests(
    root: &Path,
    files: &[File],
    tests: &mut [TestTarget],
) -> Vec<QualityNote> {
    let python_paths = files
        .iter()
        .filter(|file| file.language == Language::Python)
        .map(|file| (&file.id, file.path.as_path()))
        .collect::<HashMap<&FileId, &Path>>();
    let mut configs = HashMap::<PathBuf, Option<PytestConfig>>::new();
    let mut widened = BTreeMap::<PathBuf, (Vec<&'static str>, usize)>::new();
    for test in tests.iter_mut() {
        if test.origin != TestTargetOrigin::TestFileHelper {
            continue;
        }
        let Some(path) = python_paths.get(&test.file_id) else {
            continue;
        };
        let Some(config) = governing_config(root, path, &mut configs) else {
            continue;
        };
        if config.discovery_options.is_empty() {
            continue;
        }
        widen(test);
        widened
            .entry(config.path.clone())
            .or_insert_with(|| (config.discovery_options.clone(), 0))
            .1 += 1;
    }
    widened
        .into_iter()
        .map(|(path, (options, count))| {
            QualityNote::new(
                QualityNoteKind::TestDiscovery,
                format!(
                    "pytest configuration `{}` sets {}, which this index does not interpret; {count} Python test-file callable(s) under it that match no default discovery rule are counted as tests by the test-path rule, so helpers there count as validation too",
                    path.display(),
                    options.join(", ")
                ),
            )
        })
        .collect()
}

fn widen(test: &mut TestTarget) {
    test.origin = TestTargetOrigin::TestFileSymbol;
    test.confidence = Confidence::High;
    test.reason = CONFIGURED_DISCOVERY_REASON.into();
    test.score_breakdown = vec![ScoreComponent::single(
        "indexed_test_confidence",
        Confidence::High.score(),
        test.evidence_refs.clone(),
        CONFIGURED_DISCOVERY_REASON,
    )];
}

/// The nearest pytest configuration above `path`: the first directory, walking up to the
/// repository root, holding a configuration file with a pytest section. A directory's answer is
/// cached, since every test file under it asks the same question.
fn governing_config(
    root: &Path,
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<PytestConfig>>,
) -> Option<PytestConfig> {
    let mut walked = Vec::new();
    let mut directory = path.parent();
    let mut found = None;
    while let Some(current) = directory {
        if let Some(cached) = cache.get(current) {
            found = cached.clone();
            break;
        }
        walked.push(current.to_path_buf());
        if let Some(config) = config_in(root, current) {
            found = Some(config);
            break;
        }
        directory = current.parent();
    }
    for directory in walked {
        cache.insert(directory, found.clone());
    }
    found
}

fn config_in(root: &Path, directory: &Path) -> Option<PytestConfig> {
    PYTEST_CONFIG_FILES.iter().find_map(|(name, section)| {
        let path = directory.join(name);
        let content = fs::read_to_string(root.join(&path)).ok()?;
        let options = discovery_options(&content, section)?;
        Some(PytestConfig {
            path,
            discovery_options: options,
        })
    })
}

/// The discovery options set in `section` of an INI or TOML file, or `None` when the section is
/// absent (the file is then not a pytest configuration).
fn discovery_options(content: &str, section: &str) -> Option<Vec<&'static str>> {
    let mut lines = content.lines().map(str::trim);
    lines.find(|line| *line == section)?;
    let body = lines
        .take_while(|line| !line.starts_with('['))
        .collect::<Vec<_>>();
    Some(
        PYTEST_DISCOVERY_OPTIONS
            .into_iter()
            .filter(|option| {
                body.iter().any(|line| {
                    line.strip_prefix(option)
                        .is_some_and(|rest| rest.trim_start().starts_with('='))
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{LineRange, RepositoryId, TestSelectionTier};

    #[test]
    fn discovery_options_are_read_from_the_pytest_section_only() {
        let ini = "[pytest]\npython_files = *_tests.py check_*.py\naddopts = -q\n[coverage:run]\npython_functions = nope\n";
        assert_eq!(
            discovery_options(ini, "[pytest]"),
            Some(vec!["python_files"])
        );
        let toml = "[project]\nname = \"ledger\"\n\n[tool.pytest.ini_options]\npython_classes = [\"*Suite\"]\npython_functions = [\"should_*\"]\n";
        assert_eq!(
            discovery_options(toml, "[tool.pytest.ini_options]"),
            Some(vec!["python_classes", "python_functions"])
        );
        assert_eq!(
            discovery_options("[pytest]\naddopts = -q\n", "[pytest]"),
            Some(vec![])
        );
        assert_eq!(
            discovery_options("[flake8]\nmax-line-length = 99\n", "[tool:pytest]"),
            None
        );
    }

    fn python_file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Python,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn helper(id: &str, file: &File) -> TestTarget {
        TestTarget {
            id: id.into(),
            name: id.into(),
            file_id: file.id.clone(),
            range: Some(LineRange { start: 1, end: 2 }),
            command: None,
            confidence: Confidence::Low,
            reason: "helper".into(),
            evidence_refs: vec![id.into()],
            score_breakdown: Vec::new(),
            selection_tier: TestSelectionTier::default(),
            tier_justification: Vec::new(),
            origin: TestTargetOrigin::TestFileHelper,
        }
    }

    /// A configured project's helpers are widened back to tests, with a note; a sibling project
    /// whose pytest section sets no discovery option keeps the default rules.
    #[test]
    fn only_targets_under_a_configuration_that_changes_discovery_are_widened() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("billing/tests")).unwrap();
        fs::create_dir_all(repo.path().join("ledger/tests")).unwrap();
        fs::write(
            repo.path().join("billing/pytest.ini"),
            "[pytest]\npython_functions = should_*\n",
        )
        .unwrap();
        fs::write(
            repo.path().join("ledger/setup.cfg"),
            "[tool:pytest]\naddopts = -q\n",
        )
        .unwrap();
        fs::write(
            repo.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\npython_files = check_*.py\n",
        )
        .unwrap();
        let billing = python_file("billing", "billing/tests/check_totals.py");
        let ledger = python_file("ledger", "ledger/tests/helpers.py");
        let mut tests = vec![
            helper("should_total", &billing),
            helper("make_entry", &ledger),
        ];

        let notes = widen_configured_python_tests(repo.path(), &[billing, ledger], &mut tests);

        assert_eq!(tests[0].origin, TestTargetOrigin::TestFileSymbol);
        assert!(tests[0].counts_as_validation_evidence());
        assert_eq!(tests[1].origin, TestTargetOrigin::TestFileHelper);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].kind, QualityNoteKind::TestDiscovery);
        assert!(
            notes[0].message.contains("billing/pytest.ini")
                && notes[0].message.contains("python_functions"),
            "{}",
            notes[0].message
        );
    }
}
