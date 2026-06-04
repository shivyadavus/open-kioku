use open_kioku_core::{Confidence, File, FileId, TestTarget};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct TestSelector<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> TestSelector<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    fn get_tests_for_path(
        &self,
        path: &Path,
        test_files_with_overlap: &std::collections::HashSet<open_kioku_core::FileId>,
    ) -> Result<Vec<TestTarget>> {
        let mut file_ids = test_files_with_overlap.clone();

        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if changed_stem.len() > 3 {
            if let Ok(files) = self.store.find_files_by_path_pattern(changed_stem) {
                for f in files {
                    file_ids.insert(f.id);
                }
            }
        }

        if let Some(parent) = path.parent() {
            let parent_str = parent.to_string_lossy();
            if parent.components().count() >= 3 && parent_str.len() > 10 {
                if let Ok(files) = self.store.find_files_by_path_pattern(&parent_str) {
                    for f in files {
                        file_ids.insert(f.id);
                    }
                }
            }
        }

        let file_ids_vec = file_ids.into_iter().collect::<Vec<_>>();
        self.store.tests_for_files(&file_ids_vec)
    }

    pub fn for_changed_path(&self, path: &Path, limit: usize) -> Result<Vec<TestTarget>> {
        let files = self.store.list_files(usize::MAX, 0)?;
        let files_by_id = files
            .iter()
            .map(|file| (&file.id, file))
            .collect::<HashMap<_, _>>();
        let repo_root = self.repo_root()?;
        let tests = self.get_tests_for_path(path, &std::collections::HashSet::new())?;
        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut scored = Vec::new();
        for test in tests {
            let test_file = files_by_id.get(&test.file_id);
            let mut candidate = test;
            let test_path = test_file.map(|file| file.path.as_path());
            enhance_test_command(&repo_root, path, &mut candidate, test_path);
            let mut score = candidate.confidence.score();
            if let Some(file) = test_file {
                let test_path = file.path.to_string_lossy().to_ascii_lowercase();
                if test_path.contains(&changed_stem) {
                    score += 0.35;
                    candidate.reason =
                        format!("{}; test path matches changed file stem", candidate.reason);
                }
                if same_parent(path, &file.path) {
                    score += 0.2;
                    candidate.reason = format!("{}; same directory or package", candidate.reason);
                }
            }
            candidate.confidence = if score > 0.85 {
                Confidence::High
            } else if score > 0.55 {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            scored.push((score, candidate));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .map(|(_, test)| test)
            .take(limit)
            .collect())
    }

    pub fn for_changed_path_fast(&self, path: &Path, limit: usize) -> Result<Vec<TestTarget>> {
        let repo_root = self.repo_root()?;
        let files_by_id = if repo_root.is_some() {
            self.files_by_id().unwrap_or_default()
        } else {
            HashMap::new()
        };
        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let changed_path = path.to_string_lossy().to_ascii_lowercase();
        let path_tokens = path_tokens(&changed_path);
        let mut scored = Vec::new();
        for mut test in self.get_tests_for_path(path, &std::collections::HashSet::new())? {
            let test_path = files_by_id
                .get(&test.file_id)
                .map(|file| file.path.as_path());
            enhance_test_command(&repo_root, path, &mut test, test_path);
            let searchable = format!(
                "{} {} {} {}",
                test.id,
                test.name,
                test.command.as_deref().unwrap_or_default(),
                test.reason
            )
            .to_ascii_lowercase();
            let mut score = test.confidence.score();
            if !changed_stem.is_empty() && searchable.contains(&changed_stem) {
                score += 0.35;
                test.reason = format!("{}; test metadata matches changed file stem", test.reason);
            }
            if path_tokens.iter().any(|token| searchable.contains(token)) {
                score += 0.15;
                test.reason = format!("{}; test metadata shares path token", test.reason);
            }
            test.confidence = if score > 0.85 {
                Confidence::High
            } else if score > 0.55 {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            scored.push((score, test));
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        Ok(scored
            .into_iter()
            .map(|(_, test)| test)
            .take(limit)
            .collect())
    }

    pub fn for_changed_path_with_evidence(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<TestTarget>> {
        let changed_file = self.store.get_file_by_path(path)?;
        let repo_root = self.repo_root()?;
        let files_by_id = self.files_by_id().unwrap_or_default();
        let changed_symbols = if let Some(file) = &changed_file {
            self.store
                .occurrences_for_file(&file.id)?
                .into_iter()
                .map(|occurrence| occurrence.symbol_id)
                .collect::<std::collections::HashSet<_>>()
        } else {
            std::collections::HashSet::new()
        };
        if changed_symbols.is_empty() {
            return self.for_changed_path_fast(path, limit);
        }

        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let changed_path = path.to_string_lossy().to_ascii_lowercase();
        let path_tokens = path_tokens(&changed_path);
        let mut test_files_with_overlap = std::collections::HashSet::new();
        for symbol_id in &changed_symbols {
            if let Ok(occurrences) = self.store.references_for_symbol(symbol_id, 100) {
                for occ in occurrences {
                    if !occ.is_definition {
                        test_files_with_overlap.insert(occ.file_id.clone());
                    }
                }
            }
        }

        let mut test_symbols_by_file = std::collections::HashMap::new();
        for file_id in &test_files_with_overlap {
            let test_symbols = self
                .store
                .occurrences_for_file(file_id)?
                .into_iter()
                .map(|occurrence| occurrence.symbol_id)
                .collect::<std::collections::HashSet<_>>();
            test_symbols_by_file.insert(file_id.clone(), test_symbols);
        }

        let mut scored = Vec::new();
        for mut test in self.get_tests_for_path(path, &test_files_with_overlap)? {
            let test_path = files_by_id
                .get(&test.file_id)
                .map(|file| file.path.as_path());
            enhance_test_command(&repo_root, path, &mut test, test_path);
            let overlap = if let Some(test_symbols) = test_symbols_by_file.get(&test.file_id) {
                test_symbols
                    .iter()
                    .filter(|symbol| changed_symbols.contains(*symbol))
                    .count()
            } else {
                0
            };
            let searchable = format!(
                "{} {} {} {}",
                test.id,
                test.name,
                test.command.as_deref().unwrap_or_default(),
                test.reason
            )
            .to_ascii_lowercase();
            let mut score = test.confidence.score();
            if overlap > 0 {
                score += 0.5 + (overlap.min(5) as f32 * 0.05);
                test.reason = format!(
                    "{}; exact symbol-reference overlap with changed file ({overlap})",
                    test.reason
                );
            }
            if !changed_stem.is_empty() && searchable.contains(&changed_stem) {
                score += 0.2;
                test.reason = format!("{}; test metadata matches changed file stem", test.reason);
            }
            if path_tokens.iter().any(|token| searchable.contains(token)) {
                score += 0.1;
                test.reason = format!("{}; test metadata shares path token", test.reason);
            }
            test.confidence = if score > 0.85 {
                Confidence::High
            } else if score > 0.55 {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            scored.push((score, test));
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        Ok(scored
            .into_iter()
            .map(|(_, test)| test)
            .take(limit)
            .collect())
    }

    fn repo_root(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .store
            .manifest()?
            .map(|manifest| manifest.repository.root))
    }

    fn files_by_id(&self) -> Result<HashMap<FileId, File>> {
        Ok(self
            .store
            .list_files(usize::MAX, 0)?
            .into_iter()
            .map(|file| (file.id.clone(), file))
            .collect())
    }
}

fn same_parent(left: &Path, right: &Path) -> bool {
    left.parent().is_some() && left.parent() == right.parent()
}

fn path_tokens(path: &str) -> Vec<String> {
    path.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| token.len() >= 4)
        .take(8)
        .map(ToOwned::to_owned)
        .collect()
}

fn enhance_test_command(
    repo_root: &Option<PathBuf>,
    changed_path: &Path,
    test: &mut TestTarget,
    test_path: Option<&Path>,
) {
    let Some(root) = repo_root else {
        return;
    };
    let candidate_path = test_path.unwrap_or(changed_path);
    if let Some(command) = gradle_test_command(root, candidate_path, &test.name) {
        if test.command.as_deref() != Some(command.as_str()) {
            test.command = Some(command);
            if !test.reason.contains("Gradle-scoped") {
                test.reason = format!("{}; Gradle-scoped test command", test.reason);
            }
        }
    }
}

fn gradle_test_command(root: &Path, test_path: &Path, test_name: &str) -> Option<String> {
    if !(root.join("settings.gradle").exists()
        || root.join("settings.gradle.kts").exists()
        || root.join("build.gradle").exists()
        || root.join("build.gradle.kts").exists())
    {
        return None;
    }
    let path = test_path.to_string_lossy().replace('\\', "/");
    if !path.ends_with(".java") {
        return None;
    }
    let project_dir = nearest_gradle_project(root, test_path)?;
    let task = gradle_task_for_path(&path);
    let project = gradle_project_path(&project_dir);
    let class_filter = java_class_filter(&path).unwrap_or_else(|| test_name.to_string());
    let task_path = if project == ":" {
        format!(":{task}")
    } else {
        format!("{project}:{task}")
    };
    Some(format!("./gradlew {task_path} --tests {class_filter}"))
}

fn nearest_gradle_project(root: &Path, test_path: &Path) -> Option<PathBuf> {
    let absolute_path = if test_path.is_absolute() {
        test_path.to_path_buf()
    } else {
        root.join(test_path)
    };
    let mut current = absolute_path.parent()?.to_path_buf();
    while current.starts_with(root) {
        if current.join("build.gradle").exists() || current.join("build.gradle.kts").exists() {
            return current.strip_prefix(root).ok().map(Path::to_path_buf);
        }
        current = current.parent()?.to_path_buf();
    }
    None
}

fn gradle_project_path(project_dir: &Path) -> String {
    let mut project = String::new();
    for component in project_dir.components() {
        let value = component.as_os_str().to_string_lossy();
        if !value.is_empty() {
            project.push(':');
            project.push_str(&value);
        }
    }
    if project.is_empty() {
        ":".into()
    } else {
        project
    }
}

fn gradle_task_for_path(path: &str) -> &'static str {
    if path.contains("/src/internalClusterTest/") {
        "internalClusterTest"
    } else if path.contains("/src/javaRestTest/") {
        "javaRestTest"
    } else if path.contains("/src/yamlRestTest/") {
        "yamlRestTest"
    } else if path.contains("/src/qa/") || path.ends_with("IT.java") {
        "internalClusterTest"
    } else {
        "test"
    }
}

fn java_class_filter(path: &str) -> Option<String> {
    let marker = "/java/";
    let start = path.find(marker)? + marker.len();
    let rel = path[start..]
        .strip_suffix(".java")
        .unwrap_or(&path[start..]);
    Some(rel.replace('/', "."))
}

#[cfg(test)]
mod tests {
    use super::TestSelector;
    use open_kioku_core::Confidence;
    use open_kioku_core::{
        CodeChunk, EvidenceSourceType, File, FileId, Import, IndexManifest, Language, LineRange,
        RepositoryId, Symbol, SymbolId, SymbolOccurrence, TestTarget,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{IndexData, MetadataStore};
    use std::path::Path;

    struct FastStore {
        tests: Vec<TestTarget>,
    }

    struct EvidenceStore {
        files: Vec<File>,
        tests: Vec<TestTarget>,
        occurrences: Vec<SymbolOccurrence>,
    }

    impl MetadataStore for FastStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, _limit: usize, _offset: usize) -> Result<Vec<File>> {
            panic!("fast selector must not load the full file table")
        }

        fn get_file_by_path(&self, _path: &Path) -> Result<Option<File>> {
            Ok(None)
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, _file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(self.tests.clone())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn find_files_by_path_pattern(&self, _pattern: &str) -> Result<Vec<File>> {
            Ok(Vec::new())
        }

        fn tests_for_files(&self, file_ids: &[FileId]) -> Result<Vec<TestTarget>> {
            if file_ids.is_empty() {
                Ok(self.tests.clone())
            } else {
                let set = file_ids.iter().collect::<std::collections::HashSet<_>>();
                Ok(self
                    .tests
                    .iter()
                    .filter(|t| set.contains(&t.file_id))
                    .cloned()
                    .collect())
            }
        }

        fn references_for_symbol(
            &self,
            _id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }

        fn occurrences_for_file(&self, _file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }
    }

    impl MetadataStore for EvidenceStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, _limit: usize, _offset: usize) -> Result<Vec<File>> {
            Ok(self.files.clone())
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok(self.files.iter().find(|file| file.path == path).cloned())
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, _file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(self.tests.clone())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn find_files_by_path_pattern(&self, pattern: &str) -> Result<Vec<File>> {
            let lower_pattern = pattern.to_ascii_lowercase();
            Ok(self
                .files
                .iter()
                .filter(|f| {
                    f.path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&lower_pattern)
                })
                .cloned()
                .collect())
        }

        fn tests_for_files(&self, file_ids: &[FileId]) -> Result<Vec<TestTarget>> {
            if file_ids.is_empty() {
                Ok(self.tests.clone())
            } else {
                let set = file_ids.iter().collect::<std::collections::HashSet<_>>();
                Ok(self
                    .tests
                    .iter()
                    .filter(|t| set.contains(&t.file_id))
                    .cloned()
                    .collect())
            }
        }

        fn references_for_symbol(
            &self,
            id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(self
                .occurrences
                .iter()
                .filter(|occurrence| &occurrence.symbol_id == id)
                .cloned()
                .collect())
        }

        fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(self
                .occurrences
                .iter()
                .filter(|occurrence| &occurrence.file_id == file_id)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn fast_selector_does_not_load_all_files() {
        let store = FastStore {
            tests: vec![TestTarget {
                id: "search-service-test".into(),
                name: "SearchServiceTests".into(),
                file_id: FileId::new("test-file"),
                range: None,
                command: Some("gradle :server:test".into()),
                confidence: Confidence::Medium,
                reason: "test-like path".into(),
            }],
        };

        let selected = TestSelector::new(&store)
            .for_changed_path_fast(
                Path::new("server/src/main/java/org/foo/search/SearchService.java"),
                5,
            )
            .unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "SearchServiceTests");
        assert_eq!(selected[0].confidence, Confidence::High);
    }

    #[test]
    fn path_tokenization_keeps_searchable_segments() {
        let tokens = super::path_tokens("server/src/main/java/search/searchservice.java");
        assert!(tokens.contains(&"server".to_string()));
        assert!(tokens.contains(&"searchservice".to_string()));
        assert!(!tokens.contains(&"src".to_string()));
    }

    #[test]
    fn exact_symbol_overlap_promotes_relevant_tests() {
        let source_file = file("source", "src/auth.rs");
        let matching_test = file("matching-test", "tests/login_flow.rs");
        let unrelated_test = file("unrelated-test", "tests/billing_flow.rs");
        let store = EvidenceStore {
            files: vec![
                source_file.clone(),
                matching_test.clone(),
                unrelated_test.clone(),
            ],
            tests: vec![
                test("billing_flow", &unrelated_test.id),
                test("login_flow", &matching_test.id),
            ],
            occurrences: vec![
                occurrence("issue_token", &source_file.id),
                occurrence("issue_token", &matching_test.id),
                occurrence("charge_card", &unrelated_test.id),
            ],
        };

        let selected = TestSelector::new(&store)
            .for_changed_path_with_evidence(Path::new("src/auth.rs"), 2)
            .unwrap();

        assert_eq!(selected[0].name, "login_flow");
        assert!(selected[0]
            .reason
            .contains("exact symbol-reference overlap"));
    }

    #[test]
    fn gradle_command_scopes_elasticsearch_java_tests() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("settings.gradle"), "").unwrap();
        std::fs::create_dir_all(root.join("x-pack/plugin/ml")).unwrap();
        std::fs::write(root.join("x-pack/plugin/ml/build.gradle"), "").unwrap();

        let command = super::gradle_test_command(
            root,
            Path::new("x-pack/plugin/ml/src/test/java/org/elasticsearch/xpack/ml/inference/assignment/planning/AssignmentPlannerTests.java"),
            "AssignmentPlannerTests",
        )
        .unwrap();

        assert_eq!(
            command,
            "./gradlew :x-pack:plugin:ml:test --tests org.elasticsearch.xpack.ml.inference.assignment.planning.AssignmentPlannerTests"
        );
    }

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Rust,
            size_bytes: 10,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn test(name: &str, file_id: &FileId) -> TestTarget {
        TestTarget {
            id: name.into(),
            name: name.into(),
            file_id: file_id.clone(),
            range: None,
            command: Some("cargo test".into()),
            confidence: Confidence::Low,
            reason: "test target".into(),
        }
    }

    fn occurrence(symbol: &str, file_id: &FileId) -> SymbolOccurrence {
        SymbolOccurrence {
            symbol_id: SymbolId::new(symbol),
            file_id: file_id.clone(),
            range: Some(LineRange::single(1)),
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::Scip,
        }
    }
}
