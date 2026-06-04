use open_kioku_core::{Confidence, TestTarget};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use std::collections::HashMap;
use std::path::Path;

pub struct TestSelector<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> TestSelector<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    pub fn for_changed_path(&self, path: &Path, limit: usize) -> Result<Vec<TestTarget>> {
        let files = self.store.list_files(usize::MAX, 0)?;
        let files_by_id = files
            .iter()
            .map(|file| (&file.id, file))
            .collect::<HashMap<_, _>>();
        let tests = self.store.tests()?;
        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut scored = Vec::new();
        for test in tests {
            let test_file = files_by_id.get(&test.file_id);
            let mut candidate = test;
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
        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let changed_path = path.to_string_lossy().to_ascii_lowercase();
        let path_tokens = path_tokens(&changed_path);
        let mut scored = Vec::new();
        for mut test in self.store.tests()? {
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

#[cfg(test)]
mod tests {
    use super::TestSelector;
    use open_kioku_core::Confidence;
    use open_kioku_core::{
        CodeChunk, File, FileId, Import, IndexManifest, Symbol, SymbolId, SymbolOccurrence,
        TestTarget,
    };
    use open_kioku_errors::Result;
    use open_kioku_storage::{IndexData, MetadataStore};
    use std::path::Path;

    struct FastStore {
        tests: Vec<TestTarget>,
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
                Path::new("server/src/main/java/org/elasticsearch/search/SearchService.java"),
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
}
