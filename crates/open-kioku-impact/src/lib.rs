use chrono::Utc;
use open_kioku_core::{
    Confidence, Evidence, EvidenceId, EvidenceSourceType, FileRange, ImpactReport, RiskReport,
};
use open_kioku_errors::Result;
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{MetadataStore, SearchIndex};
use std::path::Path;

pub struct ImpactEngine<'a> {
    store: &'a dyn MetadataStore,
    search_index: Option<&'a dyn SearchIndex>,
}

impl<'a> ImpactEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self {
            store,
            search_index: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn for_file(&self, path: &Path) -> Result<ImpactReport> {
        let file = self.store.get_file_by_path(path)?;
        let target_symbols = if let Some(file) = &file {
            self.store.symbols_for_file(&file.id)?
        } else {
            Vec::new()
        };

        let direct = if let Some(file) = &file {
            let mut direct = Vec::new();
            for term in impact_terms(path, file, &target_symbols)
                .into_iter()
                .take(8)
            {
                let results = if let Some(index) = self.search_index {
                    index.search(&term, 25)?
                } else {
                    let files = self.store.list_files(usize::MAX, 0)?;
                    let chunks = self.store.all_chunks()?;
                    let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
                    search_chunks(&chunks, &files, &symbols, &term, 25)?
                };
                direct.extend(
                    results
                        .into_iter()
                        .filter(|result| result.path != file.path),
                );
            }
            let mut seen = std::collections::HashSet::new();
            direct.retain(|result| {
                seen.insert(format!(
                    "{}:{}-{}",
                    result.path.display(),
                    result
                        .line_range
                        .as_ref()
                        .map(|range| range.start)
                        .unwrap_or_default(),
                    result
                        .line_range
                        .as_ref()
                        .map(|range| range.end)
                        .unwrap_or_default()
                ))
            });
            direct.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            direct.truncate(25);
            direct
        } else {
            Vec::new()
        };

        // Second-level: for each direct impact, search for that file's stem
        // to find indirect dependents (callers-of-callers).
        let mut indirect: Vec<open_kioku_core::SearchResult> = Vec::new();
        let direct_paths: std::collections::HashSet<_> =
            direct.iter().map(|r| r.path.clone()).collect();
        for direct_result in direct.iter().take(5) {
            let indirect_stem = direct_result
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if indirect_stem.is_empty() || indirect_stem.len() < 3 {
                continue;
            }
            let second = if let Some(index) = self.search_index {
                index.search(indirect_stem, 10)?
            } else {
                let files = self.store.list_files(usize::MAX, 0)?;
                let chunks = self.store.all_chunks()?;
                let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
                search_chunks(&chunks, &files, &symbols, indirect_stem, 10)?
            };
            for result in second {
                if result.path != path && !direct_paths.contains(&result.path) {
                    indirect.push(result);
                }
            }
        }
        indirect.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        indirect.dedup_by(|a, b| a.path == b.path);
        indirect.truncate(15);
        let mut reasons = Vec::new();
        if direct.len() > 10 {
            reasons.push("many lexical dependents reference this file or its symbols".into());
        }
        if path.to_string_lossy().contains("api") {
            reasons.push("API-layer path suggests public integration surface".into());
        }
        if reasons.is_empty() {
            reasons.push("limited indexed downstream references found".into());
        }
        let score = (direct.len() as f32 / 20.0).min(1.0);
        let evidence = Evidence {
            id: EvidenceId::new(format!("impact:{}", path.display())),
            source: "open-kioku-impact".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: Some(FileRange {
                path: path.to_path_buf(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: if file.is_some() {
                Confidence::Medium
            } else {
                Confidence::Low
            },
            message: "impact report derived from indexed symbols and lexical references".into(),
            indexed_at: Utc::now(),
        };
        Ok(ImpactReport {
            target: path.display().to_string(),
            direct_impacts: direct,
            indirect_impacts: indirect,
            risk_report: RiskReport {
                level: if score > 0.6 {
                    "high"
                } else if score > 0.25 {
                    "medium"
                } else {
                    "low"
                }
                .into(),
                score,
                reasons,
            },
            evidence: vec![evidence],
        })
    }
}

fn impact_terms(
    path: &Path,
    file: &open_kioku_core::File,
    symbols: &[open_kioku_core::Symbol],
) -> Vec<String> {
    let mut terms = symbols
        .iter()
        .filter(|symbol| symbol.file_id == file.id)
        .filter(|symbol| !is_generic_symbol_name(&symbol.name))
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>();

    if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
        if !is_generic_symbol_name(stem) {
            terms.push(stem.into());
        }
    }

    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

fn is_generic_symbol_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "args"
            | "cli"
            | "command"
            | "commands"
            | "config"
            | "from"
            | "helpers"
            | "index"
            | "lib"
            | "main"
            | "mod"
            | "output"
            | "path"
            | "repo"
            | "run"
            | "test"
            | "tests"
            | "to"
            | "types"
            | "utils"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        CodeChunk, File, FileId, IndexManifest, IndexQuality, Language, Repository, RepositoryId,
    };
    use open_kioku_storage::IndexData;
    use open_kioku_storage_sqlite::SqliteStore;
    use std::path::PathBuf;

    fn make_store() -> SqliteStore {
        SqliteStore::open(":memory:").unwrap()
    }

    #[test]
    fn derives_impacts_from_chunks() {
        let store = make_store();

        let manifest = IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 0,
            chunk_count: 2,
            indexed_at: Utc::now(),
            schema_version: 1,
            quality: IndexQuality::default(),
        };

        let f1 = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/core.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f2 = File {
            id: FileId::new("f2"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/app.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f3 = File {
            id: FileId::new("f3"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };

        let c1 = CodeChunk {
            id: "c1".into(),
            file_id: FileId::new("f2"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::core::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };
        let c2 = CodeChunk {
            id: "c2".into(),
            file_id: FileId::new("f3"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::app::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[f1, f2, f3],
                symbols: &[],
                occurrences: &[],
                chunks: &[c1, c2],
                imports: &[],
                tests: &[],
            })
            .unwrap();

        let engine = ImpactEngine::new(&store);

        let report = engine.for_file(Path::new("src/core.rs")).unwrap();

        // core is referenced by app (c1), so app is direct.
        assert_eq!(report.direct_impacts.len(), 1);
        assert_eq!(
            report.direct_impacts[0].path.display().to_string(),
            "src/app.rs"
        );

        // app is referenced by main (c2), so main is indirect.
        assert_eq!(report.indirect_impacts.len(), 1);
        assert_eq!(
            report.indirect_impacts[0].path.display().to_string(),
            "src/main.rs"
        );
    }
}
