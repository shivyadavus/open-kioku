use open_kioku_core::{Confidence, TestTarget};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
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
        let tests = self.store.tests()?;
        let changed_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut scored = Vec::new();
        for test in tests {
            let test_file = files.iter().find(|file| file.id == test.file_id);
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
}

fn same_parent(left: &Path, right: &Path) -> bool {
    left.parent().is_some() && left.parent() == right.parent()
}
